use super::tables::{BAND_ALLOCATION, CACHE_BITS, CACHE_CAPS, CACHE_INDEX, E_BANDS, LOG_N, LOG2_FRAC};
use super::{BANDS, Frame};
use crate::codec::opus::range::{FRACTION_BITS, RangeCoder};

// One bit in eighths.
pub(super) const BIT: i32 = 1 << FRACTION_BITS;
// The steps of the interpolation between two rows of the static allocation (rate.c).
const ALLOC_STEPS: u32 = 6;
pub(super) const MAX_FINE_BITS: i32 = 8;
// What the fine energy's share is offset by, in eighths of a bit (rate.h).
const FINE_OFFSET: i32 = 21;
// The halvings that search a codebook size's pulse counts (rate.h).
const LOG_MAX_PSEUDO: u32 = 6;
// The pulse counts below which every count has a codebook of its own.
const EXACT_PULSES: usize = 8;
// An encoder keeps a band from band 17 up coded while it gets more than this many sixteenths of a bit a bin, fewer where it was coded the frame before, so that bands come and go less (interp_bits2pulses).
const KEEP_DEPTH: i32 = 9;
const KEPT_DEPTH: i32 = 7;
const DEPTH_FROM: usize = 17;
const DEPTH_SHIFT: i32 = 4;
// An encoder codes the first two bands at the least.
const LEAST_CODED: usize = 2;

// What an encoder chooses of the allocation: the bands it coded the frame before (none before the first frame) and the highest band that holds sound.
#[derive(Clone, Copy, Default)]
pub(super) struct Choice {
    pub(super) last_coded: usize,
    pub(super) signal_end: usize,
}

// What the allocation gives a frame's bands (4.3.3).
pub(super) struct Allocation {
    // The bits of each band's shape, in eighths.
    pub(super) pulses: [i32; BANDS],
    pub(super) fine_quant: [i32; BANDS],
    pub(super) fine_priority: [bool; BANDS],
    // The bands from the start that are coded; those past them are skipped.
    pub(super) coded_bands: usize,
    pub(super) intensity: usize,
    pub(super) dual_stereo: bool,
    // What the caps left over, for the bands to share (4.3.4.1).
    pub(super) balance: i32,
}

// The pulses of a codebook's pseudo-pulse count: every count up to 8, then 8 steps an octave (get_pulses).
pub(super) const fn pulses(pseudo: usize) -> usize {
    if pseudo < EXACT_PULSES { pseudo } else { (EXACT_PULSES + (pseudo & 7)) << ((pseudo >> 3) - 1) }
}

// A band's codebooks at a split depth LM, -1 to 3: their count, then the bits each takes in eighths, less one.
pub(super) fn cache(band: usize, lm: i32) -> &'static [u8] {
    &CACHE_BITS[CACHE_INDEX[(lm + 1) as usize * BANDS + band] as usize..]
}

// The pseudo-pulses whose bits lie nearest to `bits` in eighths, the fewer where two lie as near (bits2pulses).
pub(super) fn bits_to_pulses(band: usize, lm: i32, bits: i32) -> usize {
    let cache = cache(band, lm);
    let bits = bits - 1;
    let (mut low, mut high) = (0, usize::from(cache[0]));
    for _ in 0..LOG_MAX_PSEUDO {
        let middle = (low + high + 1) >> 1;
        if i32::from(cache[middle]) >= bits {
            high = middle;
        } else {
            low = middle;
        }
    }
    let below = if low == 0 { -1 } else { i32::from(cache[low]) };
    if bits - below <= i32::from(cache[high]) - bits { low } else { high }
}

// The bits in eighths a band's codebook of that many pseudo-pulses takes (pulses2bits).
pub(super) fn pulses_to_bits(band: usize, lm: i32, pseudo: usize) -> i32 {
    if pseudo == 0 { 0 } else { i32::from(cache(band, lm)[pseudo]) + 1 }
}

// The most bits in eighths each band can use (init_caps, 4.3.3).
pub(super) fn caps(lm: usize, channels: usize) -> [i32; BANDS] {
    core::array::from_fn(|band| {
        let width = ((E_BANDS[band + 1] - E_BANDS[band]) << lm) as i32;
        ((i32::from(CACHE_CAPS[BANDS * (2 * lm + channels - 1) + band]) + 64) * channels as i32 * width) >> 2
    })
}

const fn width(band: usize) -> i32 {
    (E_BANDS[band + 1] - E_BANDS[band]) as i32
}

// The span of bands from the start, in bins of a 2.5 ms frame.
const fn span(start: usize, end: usize) -> i32 {
    (E_BANDS[end] - E_BANDS[start]) as i32
}

// The frame's `total` bits in eighths shared among its bands, as the boosts in `offsets` and the trim bend it, with what is coded of them: the skips, an encoder's as its `choice` says, and the intensity and dual stereo of a frame of two channels, which a decoder alone reads (compute_allocation, 4.3.3).
pub(super) fn allocate<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    offsets: &[i32; BANDS],
    cap: &[i32; BANDS],
    alloc_trim: i32,
    total: i32,
    choice: &Choice,
) -> Allocation {
    let Frame { start, end, lm, channels, .. } = *frame;
    let (c, lm_bits) = (channels as i32, lm as u32);
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= BIT { BIT } else { 0 };
    total -= skip_rsv;
    let (mut intensity_rsv, mut dual_stereo_rsv) = (0, 0);
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= BIT { BIT } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut thresh = [0; BANDS];
    let mut trim_offset = [0; BANDS];
    for band in start..end {
        thresh[band] = (c << FRACTION_BITS).max(((3 * width(band)) << lm_bits << FRACTION_BITS) >> 4);
        let tilt = (alloc_trim - 5 - lm as i32) * (end - band - 1) as i32;
        trim_offset[band] = (c * width(band) * tilt * (1 << (lm_bits + FRACTION_BITS))) >> 6;
        if width(band) << lm_bits == 1 {
            trim_offset[band] -= c << FRACTION_BITS;
        }
    }
    let static_bits =
        |row: usize, band: usize| (c * width(band) * i32::from(BAND_ALLOCATION[row][band])) << lm_bits >> 2;
    let (mut low, mut high) = (1, BAND_ALLOCATION.len() as i32 - 1);
    while low <= high {
        let middle = (low + high) >> 1;
        let (mut sum, mut done) = (0, false);
        for band in (start..end).rev() {
            let mut bits = static_bits(middle as usize, band);
            if bits > 0 {
                bits = (bits + trim_offset[band]).max(0);
            }
            bits += offsets[band];
            if bits >= thresh[band] || done {
                done = true;
                sum += bits.min(cap[band]);
            } else if bits >= c << FRACTION_BITS {
                sum += c << FRACTION_BITS;
            }
        }
        if sum > total {
            high = middle - 1;
        } else {
            low = middle + 1;
        }
    }
    let (low, high) = (low as usize - 1, low as usize);
    let mut bits1 = [0; BANDS];
    let mut bits2 = [0; BANDS];
    for band in start..end {
        let mut first = static_bits(low, band);
        let mut second = if high >= BAND_ALLOCATION.len() { cap[band] } else { static_bits(high, band) };
        if first > 0 {
            first = (first + trim_offset[band]).max(0);
        }
        if second > 0 {
            second = (second + trim_offset[band]).max(0);
        }
        if low > 0 {
            first += offsets[band];
        }
        second += offsets[band];
        if offsets[band] > 0 {
            skip_start = band;
        }
        bits1[band] = first;
        bits2[band] = (second - first).max(0);
    }
    let reserves = Reserves { skip: skip_rsv, intensity: intensity_rsv, dual_stereo: dual_stereo_rsv };
    let bounds = Bounds { skip_start, thresh, cap: *cap };
    interpolate(coder, frame, &bounds, [&bits1, &bits2], total, reserves, choice)
}

// The bits held back for the skip flag, intensity and dual stereo, in eighths.
struct Reserves {
    skip: i32,
    intensity: i32,
    dual_stereo: i32,
}

// The first band that may be skipped, and each band's least and most useful bits.
struct Bounds {
    skip_start: usize,
    thresh: [i32; BANDS],
    cap: [i32; BANDS],
}

// The bits between two rows of the allocation, at and above the lower, the skips coded, and the split of each band's bits into fine energy and shape (interp_bits2pulses).
fn interpolate<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    bounds: &Bounds,
    [bits1, bits2]: [&[i32; BANDS]; 2],
    mut total: i32,
    reserves: Reserves,
    choice: &Choice,
) -> Allocation {
    let Frame { start, end, lm, channels, .. } = *frame;
    let Bounds { skip_start, thresh, cap } = *bounds;
    let Reserves { skip: skip_rsv, intensity: mut intensity_rsv, dual_stereo: mut dual_stereo_rsv } = reserves;
    let c = channels as i32;
    let stereo = u32::from(channels > 1);
    let alloc_floor = c << FRACTION_BITS;
    let log_m = (lm as i32) << FRACTION_BITS;
    let (mut low, mut high) = (0, 1 << ALLOC_STEPS);
    for _ in 0..ALLOC_STEPS {
        let middle = (low + high) >> 1;
        let (mut sum, mut done) = (0, false);
        for band in (start..end).rev() {
            let bits = bits1[band] + ((middle * bits2[band]) >> ALLOC_STEPS);
            if bits >= thresh[band] || done {
                done = true;
                sum += bits.min(cap[band]);
            } else if bits >= alloc_floor {
                sum += alloc_floor;
            }
        }
        if sum > total {
            high = middle;
        } else {
            low = middle;
        }
    }
    let mut bits = [0; BANDS];
    let (mut sum, mut done) = (0, false);
    for band in (start..end).rev() {
        let mut share = bits1[band] + ((low * bits2[band]) >> ALLOC_STEPS);
        if share < thresh[band] && !done {
            share = if share >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        share = share.min(cap[band]);
        bits[band] = share;
        sum += share;
    }
    let mut coded_bands = end;
    loop {
        let band = coded_bands - 1;
        if band <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - sum;
        let per_coefficient = left / span(start, coded_bands);
        left -= span(start, coded_bands) * per_coefficient;
        let rem = (left - span(start, band)).max(0);
        let mut band_bits = bits[band] + per_coefficient * span(band, coded_bands) + rem;
        if band_bits >= thresh[band].max(alloc_floor + BIT) {
            let depth = match coded_bands > DEPTH_FROM {
                true if band < choice.last_coded => KEPT_DEPTH,
                true => KEEP_DEPTH,
                false => 0,
            };
            let is_kept = coded_bands <= start + LEAST_CODED
                || (band_bits > (depth * span(band, coded_bands)) << lm << FRACTION_BITS >> DEPTH_SHIFT
                    && band <= choice.signal_end);
            if coder.bit(is_kept, 1) {
                break;
            }
            sum += BIT;
            band_bits -= BIT;
        }
        sum -= bits[band] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC[band - start]);
        }
        sum += intensity_rsv;
        if band_bits >= alloc_floor {
            sum += alloc_floor;
            bits[band] = alloc_floor;
        } else {
            bits[band] = 0;
        }
        coded_bands -= 1;
    }
    let intensity =
        if intensity_rsv > 0 { start + coder.uint(0, (coded_bands + 1 - start) as u32) as usize } else { 0 };
    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = dual_stereo_rsv > 0 && coder.bit(false, 1);
    let mut left = total - sum;
    let per_coefficient = left / span(start, coded_bands);
    left -= span(start, coded_bands) * per_coefficient;
    for (band, bits) in bits.iter_mut().enumerate().take(coded_bands).skip(start) {
        *bits += per_coefficient * width(band);
    }
    for (band, bits) in bits.iter_mut().enumerate().take(coded_bands).skip(start) {
        let more = left.min(width(band));
        *bits += more;
        left -= more;
    }
    let mut fine_quant = [0; BANDS];
    let mut fine_priority = [false; BANDS];
    let mut balance = 0;
    for band in start..coded_bands {
        let n = width(band) << lm;
        bits[band] += balance;
        let mut excess;
        if n > 1 {
            excess = (bits[band] - cap[band]).max(0);
            bits[band] -= excess;
            let den = c * n + i32::from(channels == 2 && n > 2 && !dual_stereo && band < intensity);
            let nc_log_n = den * (LOG_N[band] + log_m);
            let mut offset = (nc_log_n >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << FRACTION_BITS >> 2;
            }
            if bits[band] + offset < (den * 2) << FRACTION_BITS {
                offset += nc_log_n >> 2;
            } else if bits[band] + offset < (den * 3) << FRACTION_BITS {
                offset += nc_log_n >> 3;
            }
            let mut fine = ((bits[band] + offset + (den << (FRACTION_BITS - 1))) / (den << FRACTION_BITS)).max(0);
            if c * fine > bits[band] >> FRACTION_BITS {
                fine = bits[band] >> stereo >> FRACTION_BITS;
            }
            fine = fine.min(MAX_FINE_BITS);
            fine_priority[band] = fine * (den << FRACTION_BITS) >= bits[band] + offset;
            bits[band] -= (c * fine) << FRACTION_BITS;
            fine_quant[band] = fine;
        } else {
            excess = (bits[band] - (c << FRACTION_BITS)).max(0);
            bits[band] -= excess;
            fine_priority[band] = true;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + FRACTION_BITS)).min(MAX_FINE_BITS - fine_quant[band]);
            fine_quant[band] += extra_fine;
            let extra_bits = (extra_fine * c) << FRACTION_BITS;
            fine_priority[band] = extra_bits >= excess - balance;
            excess -= extra_bits;
        }
        balance = excess;
    }
    for band in coded_bands..end {
        fine_quant[band] = bits[band] >> stereo >> FRACTION_BITS;
        bits[band] = 0;
        fine_priority[band] = fine_quant[band] < 1;
    }
    Allocation { pulses: bits, fine_quant, fine_priority, coded_bands, intensity, dual_stereo, balance }
}
