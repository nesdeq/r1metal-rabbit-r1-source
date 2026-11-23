// The concealment of the reference decoder (silk/PLC.c and PLC.h), which RFC 6716 4.4 leaves open, without its comfort noise.

use super::fixed::{inverse32_varq, lcg, rshift_round, sat16, smlabb, smlawb, smulbb, smulwb, smulww, sqrt_approx};
use super::indices::{Indices, Signal};
use super::lpc::{expand, inverse_gain};
use super::params::Params;
use super::synthesis::{GAIN_TO_Q10, LTP_HALF, filter, predict, whiten};
use super::tables::LTP_TAPS;
use super::{Channel, Layout, MOST_FRAME, MOST_LAG_MS, MOST_ORDER, MOST_SUBFRAME, UNIT_GAIN_Q16};

// What a concealment state is reset to: half a frame's lag in Q8 and subframes of 20 samples, two of them.
const RESET_SUBFRAME: usize = 20;
const RESET_SUBFRAMES: usize = 2;
const LAG_Q: u32 = 8;
// The attenuation of the pitch taps, and of the noise after voiced and after unvoiced frames, in the first lost frame and in the ones after it, in Q15.
const HARMONIC_FADE: [i32; 2] = [32_440, 31_130];
const VOICED_NOISE_FADE: [i32; 2] = [31_130, 26_214];
const UNVOICED_NOISE_FADE: [i32; 2] = [32_440, 29_491];
// The range of the sum of the pitch taps a voiced frame leaves, in Q14.
const LEAST_PITCH_GAIN: i32 = 11_469;
const MOST_PITCH_GAIN: i32 = 15_565;
// The chirp of the last LPC filter in each lost frame, 0.99 in Q16.
const LPC_CHIRP: i32 = 64_880;
// The excitation the noise is drawn from: the last 128 samples of the quieter of the last two subframes.
const NOISE_SAMPLES: usize = 128;
const NOISE_SHIFT: u32 = 25;
// The least noise of a voiced frame, 0.2 in Q14, and the range of the inverse LPC gain that scales an unvoiced frame's, 2^-3 to 2^-8.
const LEAST_VOICED_NOISE: i16 = 3277;
const HIGH_GAIN_SHIFT: u32 = 3;
const LOW_GAIN_SHIFT: u32 = 8;
const ONE_Q14: i16 = 1 << 14;
const ONE_Q30: i32 = 1 << 30;
// How much the lag grows each subframe, 0.01 in Q16.
const LAG_DRIFT: i32 = 655;
// Q of the inverse gain that scales the history, and of the taps and the fades.
const INVERSE_Q: i32 = 46;
const Q10: u32 = 10;
const Q14: u32 = 14;
const Q15: u32 = 15;
const EXCITATION_SHIFT: u32 = 8;
const LTP_SHIFT: u32 = 2;
// A fade into a good frame after lost ones: energies of 24 bits, and a slope four times as steep as one frame.
const ENERGY_BITS: i32 = 24;
const SLOPE_SHIFT: u32 = 2;
const ROOT_SHIFT: u32 = 4;
const ENERGY_TOP: u32 = 0xC000_0000;

#[derive(Clone, Copy)]
pub(super) struct Plc {
    lag: i32,
    taps: [i16; LTP_TAPS],
    lpc: [i16; MOST_ORDER],
    was_lost: bool,
    seed: i32,
    noise_scale: i16,
    energy: i32,
    energy_shift: u32,
    ltp_scale: i16,
    gains: [i32; 2],
    khz: usize,
    subframes: usize,
    subframe: usize,
}

impl Plc {
    pub(super) const fn new() -> Plc {
        Plc {
            lag: 0,
            taps: [0; LTP_TAPS],
            lpc: [0; MOST_ORDER],
            was_lost: false,
            seed: 0,
            noise_scale: 0,
            energy: 0,
            energy_shift: 0,
            ltp_scale: 0,
            gains: [UNIT_GAIN_Q16; 2],
            khz: 0,
            subframes: RESET_SUBFRAMES,
            subframe: RESET_SUBFRAME,
        }
    }

    // Starts over at a new internal rate.
    fn follow(&mut self, layout: Layout) {
        if self.khz != layout.khz() {
            (self.lag, self.gains) = ((layout.frame() as i32) << (LAG_Q - 1), [UNIT_GAIN_Q16; 2]);
            (self.subframes, self.subframe, self.khz) = (RESET_SUBFRAMES, RESET_SUBFRAME, layout.khz());
        }
    }
}

// Keeps of a good frame what a lost one after it continues.
pub(super) fn update(channel: &mut Channel, layout: Layout, indices: &Indices, params: &Params) {
    let plc = &mut channel.plc;
    plc.follow(layout);
    let (subframes, order) = (layout.subframes, layout.order());
    plc.taps = [0; LTP_TAPS];
    if indices.signal == Signal::Voiced {
        let last = subframes - 1;
        let mut gain = 0;
        for back in (0..subframes).take_while(|back| back * layout.subframe() < params.pitch[last] as usize) {
            let sum = params.taps[last - back].iter().map(|&tap| i32::from(tap)).sum::<i32>();
            if sum > gain {
                gain = sum;
                plc.lag = params.pitch[last - back] << LAG_Q;
            }
        }
        plc.taps[LTP_HALF] = gain as i16;
        if gain < LEAST_PITCH_GAIN {
            let scale = (LEAST_PITCH_GAIN << Q10) / gain.max(1);
            plc.taps = plc.taps.map(|tap| (smulbb(i32::from(tap), scale) >> Q10) as i16);
        } else if gain > MOST_PITCH_GAIN {
            let scale = (MOST_PITCH_GAIN << Q14) / gain.max(1);
            plc.taps = plc.taps.map(|tap| (smulbb(i32::from(tap), scale) >> Q14) as i16);
        }
    } else {
        plc.lag = ((layout.khz() * MOST_LAG_MS) as i32) << LAG_Q;
    }
    plc.lpc[..order].copy_from_slice(&params.lpc[1][..order]);
    plc.ltp_scale = params.ltp_scale as i16;
    plc.gains = [params.gains[subframes - 2], params.gains[subframes - 1]];
    (plc.subframes, plc.subframe) = (subframes, layout.subframe());
}

// The energy of samples and the right shift that keeps it in 30 bits (silk_sum_sqr_shift).
fn energy(samples: &[i16]) -> (i32, u32) {
    let square = |at: usize| smulbb(i32::from(samples[at]), i32::from(samples[at]));
    let (mut energy, mut shift) = (0i32, 0u32);
    let last = samples.len() - 1;
    let mut at = 0;
    while at < last {
        energy = smlabb(energy, i32::from(samples[at]), i32::from(samples[at]));
        energy = smlabb(energy, i32::from(samples[at + 1]), i32::from(samples[at + 1]));
        if energy < 0 {
            // The pair that wraps is counted again below, as silk_sum_sqr_shift counts it.
            energy = ((energy as u32) >> 2) as i32;
            shift = 2;
            break;
        }
        at += 2;
    }
    while at < last {
        let pair = smlabb(square(at), i32::from(samples[at + 1]), i32::from(samples[at + 1]));
        energy = (energy as u32).wrapping_add((pair as u32).checked_shr(shift).unwrap_or(0)) as i32;
        if energy < 0 {
            energy = ((energy as u32) >> 2) as i32;
            shift += 2;
        }
        at += 2;
    }
    if at == last {
        energy = (energy as u32).wrapping_add((square(at) as u32).checked_shr(shift).unwrap_or(0)) as i32;
    }
    if energy as u32 & ENERGY_TOP != 0 {
        energy = ((energy as u32) >> 2) as i32;
        shift += 2;
    }
    (energy, shift)
}

// Extrapolates a lost frame from the frames before it and gives the lag it ended at.
pub(super) fn conceal(channel: &mut Channel, layout: Layout, out: &mut [i16]) -> i32 {
    channel.plc.follow(layout);
    let (order, history) = (layout.order(), layout.history());
    let (least, most) = layout.lags();
    let plc = &mut channel.plc;
    let gains = plc.gains.map(|gain| gain >> GAIN_TO_Q10);
    if channel.is_reset {
        plc.lpc = [0; MOST_ORDER];
    }
    let subframe = plc.subframe;
    let mut scaled = [0i16; 2 * MOST_SUBFRAME];
    for (half, gain) in gains.iter().enumerate() {
        let first = (half + plc.subframes - 2) * subframe;
        for (value, &excitation) in scaled[half * subframe..][..subframe].iter_mut().zip(&channel.excitation[first..]) {
            *value = sat16(smulww(excitation, *gain) >> EXCITATION_SHIFT);
        }
    }
    let (early, early_shift) = energy(&scaled[..subframe]);
    let (late, late_shift) = energy(&scaled[subframe..2 * subframe]);
    let noise_end = if early.checked_shr(late_shift).unwrap_or(0) < late.checked_shr(early_shift).unwrap_or(0) {
        plc.subframes - 1
    } else {
        plc.subframes
    };
    let noise = (noise_end * subframe).saturating_sub(NOISE_SAMPLES);
    let fade = usize::from(channel.losses > 0);
    let harmonic = HARMONIC_FADE[fade];
    let voiced = channel.signal == Signal::Voiced;
    let mut noise_fade = if voiced { VOICED_NOISE_FADE[fade] } else { UNVOICED_NOISE_FADE[fade] };
    expand(&mut plc.lpc[..order], LPC_CHIRP);
    let lpc = plc.lpc;
    let lpc = &lpc[..order];
    let mut noise_scale = plc.noise_scale;
    if channel.losses == 0 {
        noise_scale = ONE_Q14;
        if voiced {
            for &tap in &plc.taps {
                noise_scale = noise_scale.wrapping_sub(tap);
            }
            noise_scale = noise_scale.max(LEAST_VOICED_NOISE);
            noise_scale = (smulbb(i32::from(noise_scale), i32::from(plc.ltp_scale)) >> Q14) as i16;
        } else {
            let inverse = inverse_gain(lpc).clamp(ONE_Q30 >> LOW_GAIN_SHIFT, ONE_Q30 >> HIGH_GAIN_SHIFT);
            noise_fade = smulwb(inverse << HIGH_GAIN_SHIFT, noise_fade) >> Q14;
        }
    }
    let mut seed = plc.seed;
    let mut lag = rshift_round(plc.lag, LAG_Q).clamp(least, most) as usize;
    let start = history - lag - order - LTP_HALF;
    let mut whitened = [0; MOST_FRAME];
    whiten(&mut whitened[start..history], &channel.history[start..history], lpc);
    let inverse = inverse32_varq(plc.gains[1], INVERSE_Q).min(i32::MAX >> 1);
    let mut ltp = [0; 2 * MOST_FRAME];
    for (state, &value) in ltp[start + order..history].iter_mut().zip(&whitened[start + order..history]) {
        *state = smulwb(inverse, i32::from(value));
    }
    let mut at = history;
    for _ in 0..layout.subframes {
        for _ in 0..layout.subframe() {
            let prediction = predict(&ltp, at, lag, &plc.taps);
            seed = lcg(seed);
            let pick = (seed >> NOISE_SHIFT) as usize & (NOISE_SAMPLES - 1);
            ltp[at] = smlawb(prediction, channel.excitation[noise + pick], i32::from(noise_scale)) << LTP_SHIFT;
            at += 1;
        }
        plc.taps = plc.taps.map(|tap| (smulbb(harmonic, i32::from(tap)) >> Q15) as i16);
        noise_scale = (smulbb(i32::from(noise_scale), noise_fade) >> Q15) as i16;
        plc.lag = smlawb(plc.lag, plc.lag, LAG_DRIFT).min(most << LAG_Q);
        lag = rshift_round(plc.lag, LAG_Q).clamp(least, most) as usize;
    }
    let base = history - MOST_ORDER;
    ltp[base..history].copy_from_slice(&channel.lpc);
    filter(&mut ltp[base..history + out.len()], lpc, gains[1], out);
    channel.lpc.copy_from_slice(&ltp[base + out.len()..][..MOST_ORDER]);
    (plc.seed, plc.noise_scale) = (seed, noise_scale);
    lag as i32
}

// Fades a good frame in from the energy of the lost frames before it, and keeps a lost frame's energy for that.
pub(super) fn glue(channel: &mut Channel, out: &mut [i16]) {
    let plc = &mut channel.plc;
    if channel.losses > 0 {
        (plc.energy, plc.energy_shift) = energy(out);
        plc.was_lost = true;
        return;
    }
    if plc.was_lost {
        let (mut energy, shift) = energy(out);
        if shift > plc.energy_shift {
            plc.energy = plc.energy.checked_shr(shift - plc.energy_shift).unwrap_or(0);
        } else {
            energy = energy.checked_shr(plc.energy_shift - shift).unwrap_or(0);
        }
        if energy > plc.energy {
            let zeros = plc.energy.leading_zeros().saturating_sub(1);
            plc.energy <<= zeros;
            energy >>= (ENERGY_BITS - zeros as i32).max(0);
            let fraction = plc.energy / energy.max(1);
            let mut gain = sqrt_approx(fraction) << ROOT_SHIFT;
            let slope = ((UNIT_GAIN_Q16 - gain) / out.len() as i32) << SLOPE_SHIFT;
            for sample in out.iter_mut() {
                *sample = smulwb(gain, i32::from(*sample)) as i16;
                gain += slope;
                if gain > UNIT_GAIN_Q16 {
                    break;
                }
            }
        }
    }
    plc.was_lost = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energies_shifted() {
        assert_eq!(energy(&[3, 4]), (25, 0));
        assert_eq!(energy(&[1, 2, 3]), (14, 0), "an odd sample is counted alone");
        let loud = [i16::MIN; 4];
        let (sum, shift) = energy(&loud);
        assert!(shift > 0 && sum as u32 & ENERGY_TOP == 0, "{sum} >> {shift}: two leading zeros");
        assert_eq!(
            i64::from(sum) << shift,
            6 << 30,
            "the pair that wraps is counted twice, as silk_sum_sqr_shift counts it"
        );
    }
}
