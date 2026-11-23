use super::allocation::{Allocation, MAX_FINE_BITS};
use super::tables::{BETA_COEF, BETA_INTRA, E_MEANS, E_PROB_MODEL, PRED_COEF, SMALL_ENERGY_ICDF};
use super::{BANDS, FLAG_ROOM, Frame, INTRA_LOGP, fits};
use crate::base::math::{floor, log2_f64, pow2_f64};
use crate::codec::opus::range::RangeCoder;

// The energies of a frame's bands, for each of two channels: log2 of their amplitude less the band's mean.
pub(super) type Energies = [[f32; BANDS]; 2];

// The Laplace model's total and the values it codes with their own frequency on each side of nought (laplace.c).
const LAPLACE_BITS: u32 = 15;
const LAPLACE_TOTAL: u32 = 1 << LAPLACE_BITS;
const LAPLACE_NMIN: u32 = 16;
// The model's probability of nought and its decay are in Q8; the coder takes them in Q15 and Q14.
const PROBABILITY_SHIFT: u32 = 7;
const DECAY_SHIFT: u32 = 6;
const DECAY_ONE: u32 = 1 << 14;
// The bits left from which the Laplace model codes an energy, and from which the small table, of 2 bits, does.
const LAPLACE_LEAST: i32 = 15;
const SMALL_LEAST: i32 = 2;
const SMALL_BITS: u32 = 2;
// The lowest energy the prediction starts from (quant_bands.c).
const FLOOR: f32 = -9.0;
// The highest a band's energy is taken at, so that its amplitude stays a float (RFC 8251 8).
const CEILING: f32 = 32.0;

// A value of the Laplace-like distribution of `probability` for nought and `decay`, in Q15 and Q14: read, or `value` written, which past the values the model holds becomes the furthest it holds (ec_laplace_decode, ec_laplace_encode).
fn laplace<C: RangeCoder>(coder: &mut C, value: i32, probability: u32, decay: u32) -> i32 {
    let found = coder.locate_bin(LAPLACE_BITS);
    let magnitude = value.unsigned_abs();
    let (mut step, mut low, mut size) = (0, 0, probability);
    let is_nonzero = found.map_or(value != 0, |found| found >= size);
    let mut is_negative = false;
    if is_nonzero {
        step = 1;
        low = size;
        size = (((LAPLACE_TOTAL - 2 * LAPLACE_NMIN - size) * (DECAY_ONE - decay)) >> LAPLACE_BITS) + 1;
        while size > 1 && found.map_or(step < magnitude, |found| found >= low + 2 * size) {
            size *= 2;
            low += size;
            size = (((size - 2) * decay) >> LAPLACE_BITS) + 1;
            step += 1;
        }
        if size <= 1 {
            // Past the decaying part each value has a frequency of one on either side, up to the total.
            let more = match found {
                Some(found) => (found - low) >> 1,
                None => (magnitude - step).min(((LAPLACE_TOTAL - low + u32::from(value < 0)) >> 1).saturating_sub(1)),
            };
            step += more;
            low += 2 * more;
        }
        is_negative = found.map_or(value < 0, |found| found < low + size);
        if !is_negative {
            low += size;
        }
    }
    coder.take_bin(low, (low + size).min(LAPLACE_TOTAL), LAPLACE_BITS);
    if is_negative { -(step as i32) } else { step as i32 }
}

// The intra flag and the coarse energies, each predicted from the frame before unless intra and from the band below (unquant_coarse_energy, quant_coarse_energy_impl, 4.3.2.1). An encoder's `want` gives the step it wants for a band and channel from the prediction, the energy before and the bits used so far; the steps coded come back.
pub(super) fn code_coarse<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    intra: bool,
    energies: &mut Energies,
    mut want: impl FnMut(usize, usize, f32, f32, i32) -> i32,
) -> [[i32; BANDS]; 2] {
    let Frame { start, end, lm, channels, .. } = *frame;
    let budget = (coder.len() as u32 * u8::BITS) as i32;
    let intra = fits(coder, FLAG_ROOM, budget) && coder.bit(intra, INTRA_LOGP);
    let model = &E_PROB_MODEL[lm][usize::from(intra)];
    let (coef, beta) = if intra { (0.0, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
    let mut prediction = [0.0f32; 2];
    let mut coded = [[0; BANDS]; 2];
    for band in start..end {
        for channel in 0..channels {
            let tell = coder.tell() as i32;
            let before = energies[channel][band];
            let predicted = coef * before.max(FLOOR) + prediction[channel];
            let wanted = if C::IS_ENCODER { want(band, channel, predicted, before, tell) } else { 0 };
            let left = budget - tell;
            let step = if left >= LAPLACE_LEAST {
                let at = 2 * band;
                let (probability, decay) = (u32::from(model[at]), u32::from(model[at + 1]));
                laplace(coder, wanted, probability << PROBABILITY_SHIFT, decay << DECAY_SHIFT)
            } else if left >= SMALL_LEAST {
                let wanted = wanted.clamp(-1, 1);
                let symbol =
                    coder.icdf(((2 * wanted) ^ -i32::from(wanted < 0)) as usize, &SMALL_ENERGY_ICDF, SMALL_BITS);
                (symbol as i32 >> 1) ^ -(symbol as i32 & 1)
            } else if left >= 1 {
                -i32::from(coder.bit(wanted < 0, 1))
            } else {
                -1
            };
            coded[channel][band] = step;
            let step = step as f32;
            energies[channel][band] = predicted + step;
            prediction[channel] += step - beta * step;
        }
    }
    coded
}

// The fine energies the allocation gives bits to (unquant_fine_energy, quant_fine_energy, 4.3.2.2); an encoder's steps are of its `errors`, which they are taken off.
pub(super) fn code_fine<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    allocation: &Allocation,
    energies: &mut Energies,
    errors: &mut Energies,
) {
    for band in frame.start..frame.end {
        let bits = allocation.fine_quant[band];
        if bits <= 0 {
            continue;
        }
        let steps = (1 << bits) as f32;
        for (energies, errors) in energies.iter_mut().zip(errors.iter_mut()).take(frame.channels) {
            let wanted = floor((errors[band] + 0.5) * steps).clamp(0.0, steps - 1.0) as u32;
            let step = coder.bits(wanted, bits as u32) as f32;
            let offset = (step + 0.5) / steps - 0.5;
            energies[band] += offset;
            errors[band] -= offset;
        }
    }
}

// A last bit of fine energy for each channel of the bands of each priority in turn, while `left` bits last (unquant_energy_finalise, quant_energy_finalise, 4.3.2.2); an encoder's bit is the sign of its error.
pub(super) fn code_final<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    allocation: &Allocation,
    mut left: i32,
    energies: &mut Energies,
    errors: &mut Energies,
) {
    let channels = frame.channels as i32;
    for priority in [false, true] {
        for band in frame.start..frame.end {
            if left < channels {
                break;
            }
            let bits = allocation.fine_quant[band];
            if bits >= MAX_FINE_BITS || allocation.fine_priority[band] != priority {
                continue;
            }
            for (energies, errors) in energies.iter_mut().zip(errors.iter_mut()).take(frame.channels) {
                let step = coder.bits(u32::from(errors[band] >= 0.0), 1) as f32;
                let offset = (step - 0.5) / (2 << bits) as f32;
                energies[band] += offset;
                errors[band] -= offset;
                left -= 1;
            }
        }
    }
}

// The energy of each band below `end` from its amplitude (amp2Log2).
pub(super) fn logs(amplitudes: &[f32; BANDS], end: usize) -> [f32; BANDS] {
    core::array::from_fn(
        |band| {
            if band < end { log2_f64(f64::from(amplitudes[band])) as f32 - E_MEANS[band] } else { 0.0 }
        },
    )
}

// The amplitude of each band of `start..end`, and nought for the others (log2Amp).
pub(super) fn amplitudes(energies: &Energies, start: usize, end: usize) -> [[f32; BANDS]; 2] {
    core::array::from_fn(|channel| {
        core::array::from_fn(|band| {
            if (start..end).contains(&band) {
                let log = (energies[channel][band] + E_MEANS[band]).min(CEILING);
                pow2_f64(f64::from(log)) as f32
            } else {
                0.0
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::opus::range::{RangeDecoder, RangeEncoder};
    use alloc::vec::Vec;

    // The encoder's side of the model (ec_laplace_encode), to put known values into a frame.
    fn encode(encoder: &mut RangeEncoder, value: i32, probability: u32, decay: u32) {
        let (mut low, mut size) = (0, probability);
        if value != 0 {
            let is_negative = value < 0;
            let magnitude = value.unsigned_abs();
            low = size;
            size = ((LAPLACE_TOTAL - 2 * LAPLACE_NMIN - size) * (DECAY_ONE - decay)) >> LAPLACE_BITS;
            let mut at = 1;
            while size > 0 && at < magnitude {
                size *= 2;
                low += size + 2;
                size = (size * decay) >> LAPLACE_BITS;
                at += 1;
            }
            assert!(size > 0, "the test keeps to the decaying part of the model");
            size += 1;
            if !is_negative {
                low += size;
            }
        }
        encoder.encode_bin(low, low + size, LAPLACE_BITS);
    }

    #[test]
    fn laplace_round_trip() {
        let values: Vec<i32> = (0..200).map(|at| (at * 7 % 13) - 6).collect();
        let (probability, decay) = (72 << PROBABILITY_SHIFT, 127 << DECAY_SHIFT);
        let mut encoder = RangeEncoder::new(400);
        for &value in &values {
            encode(&mut encoder, value, probability, decay);
        }
        let frame = encoder.finish().unwrap();
        let mut range = RangeDecoder::new(frame);
        let read: Vec<i32> = values.iter().map(|_| laplace(&mut range, 0, probability, decay)).collect();
        assert_eq!(read, values, "each value as the encoder put it");
    }

    #[test]
    fn laplace_tail_of_least_probability() {
        let (probability, decay) = (177 << PROBABILITY_SHIFT, 11 << DECAY_SHIFT);
        let mut bottom = RangeDecoder::new(&[0x00; 4]);
        assert_eq!(laplace(&mut bottom, 0, probability, decay), 0, "the lowest frequency is nought's");
        let mut top = RangeDecoder::new(&[0xFF; 4]);
        assert_eq!(
            laplace(&mut top, 0, probability, decay),
            18,
            "the highest lies in the tail of single frequencies: 4 steps of the decay and 14 pairs past them"
        );
    }

    #[test]
    fn laplace_written_and_read() {
        let values: Vec<i32> = (-60..=60).chain([-30_000, 30_000, i32::MAX, -i32::MAX]).collect();
        for (probability, decay) in [(72, 127), (177, 11), (42, 121), (22, 178)] {
            let (probability, decay) = (probability << PROBABILITY_SHIFT, decay << DECAY_SHIFT);
            let mut encoder = RangeEncoder::new(2000);
            let written: Vec<i32> =
                values.iter().map(|&value| laplace(&mut encoder, value, probability, decay)).collect();
            assert!(
                values.iter().zip(&written).all(|(value, written)| value == written || written.abs() < value.abs()),
                "a value is written as it is or as the furthest the model holds on its side"
            );
            assert_eq!(written[59..=61], [-1, 0, 1], "small values as they are");
            let frame = encoder.finish().unwrap();
            let mut decoder = RangeDecoder::new(frame);
            let read: Vec<i32> = values.iter().map(|_| laplace(&mut decoder, 0, probability, decay)).collect();
            assert_eq!(read, written, "the decoder reads what the shared model wrote");
        }
    }

    #[test]
    fn amplitudes_capped() {
        let mut energies = [[0.0; BANDS]; 2];
        energies[0][3] = 1.0;
        energies[1][5] = 1000.0;
        let amplitudes = amplitudes(&energies, 2, 6);
        assert!((amplitudes[0][3] - pow2_f64(f64::from(1.0 + E_MEANS[3])) as f32).abs() < 1.0e-3);
        assert_eq!(amplitudes[1][5], 4_294_967_296.0, "2 to the 32nd at most");
        assert_eq!((amplitudes[0][1], amplitudes[0][6]), (0.0, 0.0), "nought outside the coded bands");
    }
}
