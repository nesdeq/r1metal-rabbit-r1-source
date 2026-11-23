// An encoder of the CELT layer's frames of 20 ms from one channel at 48 kHz, at the size it is given, as libopus's float encoder codes them at its default complexity (celt_encoder.c).

use super::allocation::{Choice, allocate, caps};
use super::analysis::{self, Spreading, Tone};
use super::bands;
use super::energy::{self, Energies};
use super::mdct::Mdct;
use super::pitch;
use super::postfilter::{self, MAX_PERIOD, MIN_PERIOD, Pitch};
use super::tables::E_BANDS;
use super::{
    BANDS, BIT, FLAG_ROOM, FRAME, Frame, MAX_LM, OVERLAP, POSTFILTER_ROOM, PREEMPHASIS, QUIET, SPREAD_NORMAL,
    SPREAD_ROOM, code_boosts, code_pitch, code_silence, code_spread, code_transient, code_trim, end_of, fits, shares,
};
use crate::base::math::floor;
use crate::codec::Flaw;
use crate::codec::opus::packet::Bandwidth;
use crate::codec::opus::range::RangeEncoder;
use alloc::vec;
use alloc::vec::Vec;

// A frame of samples no larger than half a step of 16 bits is silent; samples past +6 dB of full scale are clipped before the pre-emphasis (celt_encode_with_ec).
const SILENCE_LEVEL: f32 = 0.5;
const CLIP: f32 = 65_536.0;
// The pre-filter is tried in frames of more bytes than these (celt_encode_with_ec).
const LEAST_PREFILTER_BYTES: usize = 12;
// A tone this pure has its period found from its frequency: above the post-filter's highest pitch, in radians a sample, at a multiple of it; below the lowest, at the shortest period; at a gain of 0.75 (run_prefilter).
const TONE_PERIOD_PURITY: f32 = 0.99;
const TONE_HIGHEST: f32 = 0.39;
const TONE_LOWEST: f32 = 0.006_148;
const TONE_GAIN: f32 = 0.75;
// A found period's gain is taken at 0.7 of its correlation (run_prefilter).
const SEARCH_GAIN: f32 = 0.7;
// The gain from which the pre-filter is on: higher for a change of period, a small frame, lower for a strong filter the frame before, never under 0.2; a gain this near the last is kept; a strong transient with a change of period has none (run_prefilter).
const PITCH_THRESHOLD: f32 = 0.2;
const PITCH_CHANGE: f32 = 0.2;
const PITCH_CHANGE_SHARE: i32 = 10;
const PITCH_TRANSIENT: f32 = 0.98;
const PITCH_SMALL_STEP: f32 = 0.1;
const PITCH_SMALL_BYTES: [usize; 2] = [25, 35];
const PITCH_STRONG: [f32; 2] = [0.4, 0.55];
const PITCH_KEEP: f32 = 0.1;
const PITCH_GAIN_STEPS: f32 = 32.0 / 3.0;
const PITCH_MOST_STEP: i32 = 7;
// A frame found transient by its energies rather than its samples has this tf estimate (celt_encode_with_ec).
const PATCHED_TF_ESTIMATE: f32 = 0.2;
// The tf analysis runs in frames of 15 bytes or more that hold no pure tone, its cost of a change in tf being at least 80 (celt_encode_with_ec).
const TF_LEAST_BYTES: i32 = 15;
const TF_TONE: f32 = 0.98;
const TF_LAMBDA_LEAST: i32 = 80;
const TF_LAMBDA_BYTES: i32 = 20_480;
const TF_LAMBDA_OFFSET: i32 = 2;
// The spreading is decided in frames of 10 bytes or more of long blocks (celt_encode_with_ec).
const SPREAD_LEAST_BYTES: usize = 10;
// Where a band's energy moved less than 2 since the frame before, a quarter of its last error is taken off, for a steadier gain (celt_encode_with_ec).
const STEADY: f32 = 2.0;
const ERROR_SHARE: f32 = 0.25;
const ERROR_MOST: f32 = 0.5;
// The coarse energy falls no faster than 16 a frame, or an eighth a byte of the frame where it has more than 10 bands; with fewer bits left than these for the bands after, three a band, its steps are held to one at the most and then to one either way (quant_coarse_energy).
const MAX_DECAY: f32 = 16.0;
const DECAY_PER_BYTE: f32 = 0.125;
const DECAY_BANDS: usize = 10;
const BITS_PER_BAND: i32 = 3;
const FEW_BITS: i32 = 30;
const FEWER_BITS: i32 = 24;
const FEWEST_BITS: i32 = 16;
// Anti-collapse is wanted unless two frames before were transient too (celt_encode_with_ec).
const MOST_TRANSIENTS: u32 = 2;
// Frames of 20 ms a second: a frame's size as the rate the trim is chosen by, which counts no overhead for frames of 20 ms (celt_encode_with_ec).
const FRAMES_PER_SECOND: i32 = 50;

pub(in crate::codec::opus) struct Encoder {
    mdct: Mdct,
    end: usize,
    preemph_mem: f32,
    overlap_max: f32,
    // The pre-filter's: the pre-emphasised samples it reaches back into, the filtered samples the next frame's transform overlaps, and its pitch.
    history: Vec<f32>,
    filtered: Vec<f32>,
    pitch: Pitch,
    // The bands' energies as coded, and their last errors.
    old_e: Energies,
    energy_error: [f32; BANDS],
    spreading: Spreading,
    transients: u32,
    last_coded: usize,
    input: Vec<f32>,
    pre: Vec<f32>,
    work: Vec<f32>,
    lowpass: Vec<f32>,
    spectrum: Vec<f32>,
    shapes: Vec<f32>,
    norm: Vec<f32>,
    // The frame's range coder, and the state the coarse energy's trial goes back to, kept so that a frame takes no memory.
    coder: RangeEncoder,
    trial: RangeEncoder,
}

impl Encoder {
    pub(in crate::codec::opus) fn new(bandwidth: Bandwidth) -> Encoder {
        Encoder {
            mdct: Mdct::new(),
            end: end_of(bandwidth),
            preemph_mem: 0.0,
            overlap_max: 0.0,
            history: vec![0.0; MAX_PERIOD],
            filtered: vec![0.0; OVERLAP],
            pitch: Pitch::default(),
            old_e: [[0.0; BANDS]; 2],
            energy_error: [0.0; BANDS],
            spreading: Spreading::default(),
            transients: 0,
            last_coded: 0,
            input: vec![0.0; OVERLAP + FRAME],
            pre: vec![0.0; MAX_PERIOD + FRAME],
            work: vec![0.0; MAX_PERIOD + FRAME],
            lowpass: vec![0.0; (MAX_PERIOD + FRAME) / 2],
            spectrum: vec![0.0; FRAME],
            shapes: vec![0.0; FRAME],
            norm: vec![0.0; 2 * (E_BANDS[BANDS] << MAX_LM)],
            coder: RangeEncoder::new(0),
            trial: RangeEncoder::new(0),
        }
    }

    // Encodes a frame of 960 samples, at the scale of 16 bits, into `out`, all of whose bytes it is coded in; the final range.
    pub(in crate::codec::opus) fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<u32, Flaw> {
        let (lm, end) = (MAX_LM, self.end);
        let m = 1 << lm;
        let bytes = out.len();
        let total_bits = (bytes as u32 * u8::BITS) as i32;
        let rate = total_bits * FRAMES_PER_SECOND;
        let mut coder = core::mem::replace(&mut self.coder, RangeEncoder::new(0));
        coder.reset(bytes);
        let largest = |samples: &[f32]| samples.iter().fold(0.0f32, |most, sample| most.max(sample.abs()));
        let (head, tail) = (largest(&pcm[..FRAME - OVERLAP]), largest(&pcm[FRAME - OVERLAP..]));
        let sample_max = self.overlap_max.max(head).max(tail);
        self.overlap_max = tail;
        let is_silent = code_silence(&mut coder, sample_max <= SILENCE_LEVEL, total_bits);
        let is_clipped = sample_max > CLIP;
        self.input[..OVERLAP].copy_from_slice(&self.history[MAX_PERIOD - OVERLAP..]);
        for (value, &sample) in self.input[OVERLAP..].iter_mut().zip(pcm) {
            let sample = if is_clipped { sample.clamp(-CLIP, CLIP) } else { sample };
            *value = sample - self.preemph_mem;
            self.preemph_mem = PREEMPHASIS * sample;
        }
        let tone = analysis::tone(&self.input);
        let (mut is_transient, mut tf_estimate) = analysis::transient(&self.input, tone);
        let tone = Tone { toneishness: tone.toneishness.min(1.0 - tf_estimate), ..tone };
        let is_enabled = bytes > LEAST_PREFILTER_BYTES && !is_silent && fits(&coder, POSTFILTER_ROOM, total_bits);
        let (wanted, next) = self.prefilter(is_enabled, tone, tf_estimate, bytes);
        code_pitch(&mut coder, wanted, total_bits);
        let has_room = fits(&coder, FLAG_ROOM, total_bits);
        is_transient &= has_room;
        // A transient frame's energies of a long block too, for its boosts; short blocks hold a long block's energy at 1/2^(LM/2) each.
        let block_gain = 0.5 * lm as f32;
        let mut long = [0.0; BANDS];
        if is_transient {
            self.mdct.forward(&self.input, 1, &mut self.spectrum);
            long = energy::logs(&bands::measure(&self.spectrum, lm, end), end);
            long.iter_mut().take(end).for_each(|energy| *energy += block_gain);
        }
        let blocks = if is_transient { m } else { 1 };
        self.mdct.forward(&self.input, blocks, &mut self.spectrum);
        let mut amplitudes = bands::measure(&self.spectrum, lm, end);
        let mut energies = energy::logs(&amplitudes, end);
        if !is_transient {
            long = energies;
        }
        if has_room && !is_transient && analysis::has_risen(&energies, &self.old_e[0], end) {
            is_transient = true;
            self.mdct.forward(&self.input, m, &mut self.spectrum);
            amplitudes = bands::measure(&self.spectrum, lm, end);
            energies = energy::logs(&amplitudes, end);
            long.iter_mut().take(end).for_each(|energy| *energy += block_gain);
            tf_estimate = PATCHED_TF_ESTIMATE;
        }
        let is_transient = code_transient(&mut coder, lm, is_transient, total_bits);
        bands::normalise(&self.spectrum, &amplitudes, lm, end, &mut self.shapes);
        let bytes_given = bytes as i32;
        let boosts = analysis::boosts(&energies, &long, end, lm, is_transient, bytes_given, tone);
        let (tf_wanted, tf_select) = if bytes_given >= TF_LEAST_BYTES && tone.toneishness < TF_TONE {
            let lambda = TF_LAMBDA_LEAST.max(TF_LAMBDA_BYTES / bytes_given + TF_LAMBDA_OFFSET);
            analysis::tf(&self.shapes, end, lm, is_transient, tf_estimate, lambda, &boosts.importance)
        } else {
            ([usize::from(is_transient); BANDS], false)
        };
        let mut targets = energies;
        for band in 0..end {
            if (energies[band] - self.old_e[0][band]).abs() < STEADY {
                targets[band] -= ERROR_SHARE * self.energy_error[band];
            }
        }
        let mut frame =
            Frame { start: 0, end, lm, channels: 1, is_transient, spread: SPREAD_NORMAL, tf_res: [0; BANDS] };
        let mut errors = self.coarse(&mut coder, &frame, &targets, bytes);
        frame.tf_res = bands::code_tf(&mut coder, &frame, &tf_wanted, tf_select);
        let spread = if !fits(&coder, SPREAD_ROOM, total_bits) || is_transient || bytes < SPREAD_LEAST_BYTES {
            SPREAD_NORMAL
        } else {
            let does_update = wanted.is_some();
            self.spreading.decide(&self.shapes, end, lm, &boosts.spread_weight, does_update)
        };
        frame.spread = code_spread(&mut coder, spread, total_bits);
        self.spreading.decision = frame.spread;
        let cap = caps(lm, 1);
        let (offsets, total) = code_boosts(&mut coder, &frame, &cap, &boosts.offsets, total_bits);
        let trim = code_trim(&mut coder, analysis::trim(&targets, end, tf_estimate, rate), total);
        let (bits, reserved) = shares(&coder, &frame, total_bits);
        let choice = Choice { last_coded: self.last_coded, signal_end: end - 1 };
        let allocation = allocate(&mut coder, &frame, &offsets, &cap, trim, bits, &choice);
        self.last_coded = if self.last_coded > 0 {
            (self.last_coded + 1).min((self.last_coded - 1).max(allocation.coded_bands))
        } else {
            allocation.coded_bands
        };
        energy::code_fine(&mut coder, &frame, &allocation, &mut self.old_e, &mut errors);
        let mut seed = 0;
        let total = total_bits * BIT - reserved;
        bands::code(&mut coder, &frame, &allocation, total, &mut self.shapes, &mut self.norm, &mut seed);
        if reserved > 0 {
            coder.bits(u32::from(self.transients < MOST_TRANSIENTS), 1);
        }
        let left = total_bits - coder.tell() as i32;
        energy::code_final(&mut coder, &frame, &allocation, left, &mut self.old_e, &mut errors);
        self.energy_error =
            core::array::from_fn(|band| if band < end { errors[0][band].clamp(-ERROR_MOST, ERROR_MOST) } else { 0.0 });
        if is_silent {
            self.old_e[0] = [QUIET; BANDS];
        }
        self.old_e[0][end..].fill(0.0);
        self.pitch = next;
        self.transients = if is_transient || !has_room { self.transients + 1 } else { 0 };
        let range = coder.range();
        let finished = coder.finish().map(|frame| out.copy_from_slice(frame));
        self.coder = coder;
        finished.ok_or(Flaw::Broken)?;
        Ok(range)
    }

    // The coarse energy coded intra and predicted, as the one that comes nearer what was wanted, or the shorter where they tie, has it; the errors of what was coded (quant_coarse_energy).
    fn coarse(&mut self, coder: &mut RangeEncoder, frame: &Frame, targets: &[f32; BANDS], bytes: usize) -> Energies {
        let Frame { start, end, .. } = *frame;
        let total_bits = (bytes as u32 * u8::BITS) as i32;
        let max_decay =
            if end - start > DECAY_BANDS { MAX_DECAY.min(DECAY_PER_BYTE * bytes as f32) } else { MAX_DECAY };
        // A pass of the coarse energy: the errors of the steps coded, and how far they lie from those wanted (quant_coarse_energy_impl).
        let pass = |coder: &mut RangeEncoder, intra: bool, energies: &mut Energies| -> (Energies, i32) {
            let (mut wanted, mut residuals) = ([[0; BANDS]; 2], [[0.0; BANDS]; 2]);
            let coded = energy::code_coarse(coder, frame, intra, energies, |band, channel, predicted, before, tell| {
                let target = targets[band];
                let residual = target - predicted;
                let mut step = floor(0.5 + residual) as i32;
                // An energy falls no faster than the decay allows, as a band of one bin otherwise would.
                let bound = before.max(QUIET) - max_decay;
                if step < 0 && target < bound {
                    step = (step + (bound - target) as i32).min(0);
                }
                (wanted[channel][band], residuals[channel][band]) = (step, residual);
                let left = total_bits - tell - BITS_PER_BAND * (end - band) as i32;
                if band != start && left < FEW_BITS {
                    if left < FEWER_BITS {
                        step = step.min(1);
                    }
                    if left < FEWEST_BITS {
                        step = step.max(-1);
                    }
                }
                step
            });
            let badness = (start..end).map(|band| (wanted[0][band] - coded[0][band]).abs()).sum();
            let errors = core::array::from_fn(|channel| {
                core::array::from_fn(|band| residuals[channel][band] - coded[channel][band] as f32)
            });
            (errors, badness)
        };
        if !fits(coder, FLAG_ROOM, total_bits) {
            return pass(coder, false, &mut self.old_e).0;
        }
        self.trial.clone_from(coder);
        let mut intra_energies = self.old_e;
        let (intra_errors, intra_badness) = pass(coder, true, &mut intra_energies);
        let intra_tell = coder.tell_frac();
        // The coder goes back to where both passes start, and the trial keeps the intra pass's.
        core::mem::swap(coder, &mut self.trial);
        let (errors, badness) = pass(coder, false, &mut self.old_e);
        if intra_badness < badness || (intra_badness == badness && coder.tell_frac() > intra_tell) {
            core::mem::swap(coder, &mut self.trial);
            self.old_e = intra_energies;
            return intra_errors;
        }
        errors
    }

    // Takes the pitch out of the frame in `input` where that pays and keeps what the next frame's filter reaches back into: the pitch to code, none where the filter is off, and the one the next frame crosses from (run_prefilter).
    fn prefilter(&mut self, is_enabled: bool, tone: Tone, tf_estimate: f32, bytes: usize) -> (Option<Pitch>, Pitch) {
        self.pre[..MAX_PERIOD].copy_from_slice(&self.history);
        self.pre[MAX_PERIOD..].copy_from_slice(&self.input[OVERLAP..]);
        let last = self.pitch;
        let (period, mut gain) = if is_enabled && tone.toneishness > TONE_PERIOD_PURITY {
            let frequency = tone.frequency;
            // A pitch too high for the post-filter is taken at a multiple of its period.
            let mut multiple = 1.0;
            while frequency >= multiple * TONE_HIGHEST {
                multiple += 1.0;
            }
            let period = if frequency > TONE_LOWEST {
                (floor(0.5 + core::f32::consts::TAU * multiple / frequency) as usize).min(MAX_PERIOD - 2)
            } else {
                MIN_PERIOD
            };
            (period, TONE_GAIN)
        } else if is_enabled {
            pitch::downsample(&self.pre, &mut self.lowpass);
            // The last 1.5 octaves of the range are not searched, where short-term correlation gives false periods.
            let most = MAX_PERIOD - 3 * MIN_PERIOD;
            let mut period = MAX_PERIOD - pitch::search(&self.lowpass[MAX_PERIOD / 2..], &self.lowpass, FRAME, most);
            let gain = pitch::remove_doubling(&self.lowpass, FRAME, &mut period, last.period, last.gain);
            (period.min(MAX_PERIOD - 2), SEARCH_GAIN * gain)
        } else {
            (MIN_PERIOD, 0.0)
        };
        let mut threshold = PITCH_THRESHOLD;
        if (period as i32 - last.period as i32).abs() * PITCH_CHANGE_SHARE > period as i32 {
            threshold += PITCH_CHANGE;
            if tf_estimate > PITCH_TRANSIENT {
                gain = 0.0;
            }
        }
        threshold += PITCH_SMALL_STEP * PITCH_SMALL_BYTES.iter().filter(|&&small| bytes < small).count() as f32;
        threshold -= PITCH_SMALL_STEP * PITCH_STRONG.iter().filter(|&&strong| last.gain > strong).count() as f32;
        let tapset = self.spreading.tapset;
        let mut wanted = None;
        if gain < threshold.max(PITCH_THRESHOLD) {
            gain = 0.0;
        } else {
            if (gain - last.gain).abs() < PITCH_KEEP {
                gain = last.gain;
            }
            let step = (floor(0.5 + gain * PITCH_GAIN_STEPS) as i32 - 1).clamp(0, PITCH_MOST_STEP);
            gain = super::GAIN_STEP * (step + 1) as f32;
            wanted = Some(Pitch { period, gain, tapset });
        }
        let from = Pitch { period: last.period.max(MIN_PERIOD), ..last };
        let window = self.mdct.window();
        let before: f32 = self.input[OVERLAP..].iter().map(|sample| sample.abs()).sum();
        self.input[..OVERLAP].copy_from_slice(&self.filtered);
        self.work.copy_from_slice(&self.pre);
        postfilter::filter(&mut self.work, MAX_PERIOD, FRAME, [from, Pitch { period, gain, tapset }], window, true);
        self.input[OVERLAP..].copy_from_slice(&self.work[MAX_PERIOD..]);
        let after: f32 = self.input[OVERLAP..].iter().map(|sample| sample.abs()).sum();
        if after > before {
            // A filter that leaves the frame louder is no help: it fades out over the overlap instead.
            self.work.copy_from_slice(&self.pre);
            let off = Pitch { period, gain: 0.0, tapset };
            postfilter::filter(&mut self.work, MAX_PERIOD, OVERLAP, [from, off], window, true);
            self.input[OVERLAP..].copy_from_slice(&self.work[MAX_PERIOD..]);
            gain = 0.0;
            wanted = None;
        }
        self.filtered.copy_from_slice(&self.input[FRAME..]);
        self.history.copy_within(FRAME.., 0);
        self.history[MAX_PERIOD - FRAME..].copy_from_slice(&self.pre[MAX_PERIOD..]);
        (wanted, Pitch { period, gain, tapset })
    }
}
