mod allocation;
mod analysis;
mod bands;
mod encode;
mod energy;
mod mdct;
mod pitch;
mod postfilter;
mod pvq;
mod tables;
#[cfg(test)]
mod tests;

pub(super) use encode::Encoder;

use super::packet::Bandwidth;
use super::range::{RangeCoder, RangeDecoder, ilog};
use crate::codec::Flaw;
use alloc::vec;
use alloc::vec::Vec;
use allocation::{BIT, Choice, allocate, caps};
use energy::Energies;
use mdct::Mdct;
use postfilter::Pitch;
use pvq::renormalise;
use tables::{E_BANDS, SPREAD_ICDF, TAPSET_ICDF, TRIM_ICDF};

// The bands of the CELT layer, the bins of its shortest transform, the samples its windows overlap by, and the LM of a 20 ms frame (4.3, Table 55).
const BANDS: usize = 21;
const SHORT: usize = 120;
const OVERLAP: usize = 120;
const MAX_LM: usize = 3;
// The frame the encoder codes: 20 ms.
pub(super) const FRAME: usize = SHORT << MAX_LM;
// The first band of a hybrid frame, above SILK's 8 kHz, and the band each narrower bandwidth ends before (4.3, Table 55).
const HYBRID_START: usize = 17;
const NARROW_END: usize = 13;
const WIDE_END: usize = 17;
const SUPER_WIDE_END: usize = 19;
// The widest band's bins: band 20 at LM 3.
const WIDEST: usize = (E_BANDS[BANDS] - E_BANDS[BANDS - 1]) << MAX_LM;
// The samples the post-filter reaches back into (DECODE_BUFFER_SIZE).
const HISTORY: usize = 2048;
// The energy a band is taken to have had before any frame, and after a silent one (celt.c).
const QUIET: f32 = -28.0;
// alpha_p of the pre-emphasis and the de-emphasis (4.3.7.2).
const PREEMPHASIS: f32 = 0.850_006_1;
// The post-filter's octaves, the period's least value and fine bits, the bits of its gain and the gain's step (4.3.7.1).
const OCTAVES: u32 = 6;
const PERIOD_BASE: u32 = 16;
const PERIOD_BITS: u32 = 4;
const GAIN_BITS: u32 = 3;
const GAIN_STEP: f32 = 3.0 / 32.0;
// The logp of the silence, post-filter, transient and intra flags, of a band's first boost at most and at least, and of its boosts after (Table 56, 4.3.3).
const SILENCE_LOGP: u32 = 15;
const POSTFILTER_LOGP: u32 = 1;
const TRANSIENT_LOGP: u32 = 3;
const INTRA_LOGP: u32 = 3;
const DYNALLOC_LOGP: u32 = 6;
const LEAST_DYNALLOC_LOGP: u32 = 2;
const NEXT_BOOST_LOGP: u32 = 1;
// A boost is 6 bits, but at least an eighth of a bit and at most one bit a bin (4.3.3).
const BOOST_BITS: i32 = 6;
// The bits of the trim's, the spread's and the tapset's tables (Table 56, 58).
const TRIM_BITS: u32 = 7;
const SPREAD_BITS: u32 = 5;
const TAPSET_BITS: u32 = 2;
// The bits a frame must have left to code the post-filter, the tapset, the transient and intra flags, the spread and the trim (celt.c).
const POSTFILTER_ROOM: i32 = 16;
const TAPSET_ROOM: i32 = 2;
const FLAG_ROOM: i32 = 3;
const SPREAD_ROOM: i32 = 4;
const TRIM_ROOM: i32 = 6 * BIT;
const SPREAD_NORMAL: usize = 2;
const TRIM_NONE: i32 = 5;
// The background noise's energy may rise this much a 2.5 ms block (celt.c).
const BACKGROUND_RISE: f32 = 0.001;
// How much a lost frame's energies fall, the first and each one after; from this many lost frames on they are the background's (celt_decode_lost).
const FIRST_LOSS_DECAY: f32 = 1.5;
const LOSS_DECAY: f32 = 0.5;
const BACKGROUND_LOSSES: u32 = 5;

// How a frame is laid out: its bands, its LM, its channels, and what its header tells of its blocks and spreading.
struct Frame {
    start: usize,
    end: usize,
    lm: usize,
    channels: usize,
    is_transient: bool,
    spread: usize,
    tf_res: [i32; BANDS],
}

// The LM of a frame of that many samples, 2.5 ms to 20 ms.
fn lm_of(samples: usize) -> Result<usize, Flaw> {
    (0..=MAX_LM).find(|&lm| SHORT << lm == samples).ok_or(Flaw::Invalid("a frame size CELT does not have"))
}

// The band a frame of that bandwidth ends before (Table 55).
const fn end_of(bandwidth: Bandwidth) -> usize {
    match bandwidth {
        Bandwidth::Narrow => NARROW_END,
        Bandwidth::Medium | Bandwidth::Wide => WIDE_END,
        Bandwidth::SuperWide => SUPER_WIDE_END,
        Bandwidth::Full => BANDS,
    }
}

// Whether the frame has `room` bits beyond those coded so far.
fn fits<C: RangeCoder>(coder: &C, room: i32, total_bits: i32) -> bool {
    coder.tell() as i32 + room <= total_bits
}

// The silence flag, which only a frame's first symbol may be; a silent frame counts every bit as used (Table 56).
fn code_silence<C: RangeCoder>(coder: &mut C, is_silent: bool, total_bits: i32) -> bool {
    let tell = coder.tell() as i32;
    let is_silent = if tell >= total_bits { true } else { tell == 1 && coder.bit(is_silent, SILENCE_LOGP) };
    if is_silent {
        coder.exhaust();
    }
    is_silent
}

// The post-filter's period, gain and tapset where the frame has room for them, an encoder's `wanted` or none (4.3.7.1).
fn code_pitch<C: RangeCoder>(coder: &mut C, wanted: Option<Pitch>, total_bits: i32) -> Pitch {
    let mut pitch = Pitch::default();
    if !fits(coder, POSTFILTER_ROOM, total_bits) || !coder.bit(wanted.is_some(), POSTFILTER_LOGP) {
        return pitch;
    }
    let wanted = wanted.unwrap_or_default();
    let index = wanted.period as u32 + 1;
    let octave = coder.uint(ilog(index).saturating_sub(ilog(PERIOD_BASE)), OCTAVES);
    let fine = coder.bits(index.saturating_sub(PERIOD_BASE << octave), PERIOD_BITS + octave);
    pitch.period = ((PERIOD_BASE << octave) + fine - 1) as usize;
    let gain = coder.bits(((wanted.gain / GAIN_STEP) as u32).saturating_sub(1), GAIN_BITS);
    if fits(coder, TAPSET_ROOM, total_bits) {
        pitch.tapset = coder.icdf(wanted.tapset, &TAPSET_ICDF, TAPSET_BITS);
    }
    pitch.gain = GAIN_STEP * (gain + 1) as f32;
    pitch
}

// The transient flag of a frame of more than one short block, where it has room (Table 56).
fn code_transient<C: RangeCoder>(coder: &mut C, lm: usize, is_transient: bool, total_bits: i32) -> bool {
    lm > 0 && fits(coder, FLAG_ROOM, total_bits) && coder.bit(is_transient, TRANSIENT_LOGP)
}

// The spread decision, normal where the frame has no room for it (Table 56, 59).
fn code_spread<C: RangeCoder>(coder: &mut C, spread: usize, total_bits: i32) -> usize {
    if fits(coder, SPREAD_ROOM, total_bits) { coder.icdf(spread, &SPREAD_ICDF, SPREAD_BITS) } else { SPREAD_NORMAL }
}

// The boosts of the dynamic allocation, an encoder's `wanted` count of quanta for each band: each band's boost and the bits left, both in eighths (4.3.3).
fn code_boosts<C: RangeCoder>(
    coder: &mut C,
    frame: &Frame,
    cap: &[i32; BANDS],
    wanted: &[i32; BANDS],
    total_bits: i32,
) -> ([i32; BANDS], i32) {
    let Frame { start, end, lm, channels, .. } = *frame;
    let mut offsets = [0; BANDS];
    let mut logp = DYNALLOC_LOGP;
    let mut total = total_bits * BIT;
    let mut tell = coder.tell_frac() as i32;
    for band in start..end {
        let width = ((channels * (E_BANDS[band + 1] - E_BANDS[band])) << lm) as i32;
        let quanta = (width * BIT).min((BOOST_BITS * BIT).max(width));
        let mut loop_logp = logp;
        let (mut boost, mut count) = (0, 0);
        while tell + (loop_logp as i32 * BIT) < total && boost < cap[band] {
            let is_boosted = coder.bit(count < wanted[band], loop_logp);
            tell = coder.tell_frac() as i32;
            if !is_boosted {
                break;
            }
            boost += quanta;
            total -= quanta;
            loop_logp = NEXT_BOOST_LOGP;
            count += 1;
        }
        offsets[band] = boost;
        if boost > 0 {
            logp = (logp - 1).max(LEAST_DYNALLOC_LOGP);
        }
    }
    (offsets, total)
}

// The allocation trim, the middle one where the frame has no room for it after the boosts, of `total` bits in eighths (Table 58).
fn code_trim<C: RangeCoder>(coder: &mut C, trim: i32, total: i32) -> i32 {
    if coder.tell_frac() as i32 + TRIM_ROOM <= total {
        coder.icdf(trim as usize, &TRIM_ICDF, TRIM_BITS) as i32
    } else {
        TRIM_NONE
    }
}

// The bits in eighths the allocation shares out and the bit held back for the anti-collapse flag, of a transient frame of four short blocks or more where it has room (4.3.5).
fn shares<C: RangeCoder>(coder: &C, frame: &Frame, total_bits: i32) -> (i32, i32) {
    let bits = total_bits * BIT - coder.tell_frac() as i32 - 1;
    let lm = frame.lm as i32;
    let reserved = if frame.is_transient && lm >= 2 && bits >= (lm + 2) * BIT { BIT } else { 0 };
    (bits - reserved, reserved)
}

// A decoder of the CELT layer's frames into one channel at 48 kHz, a frame of two channels mixed into it; its samples at the scale of 16 bits.
pub(super) struct Decoder {
    mdct: Mdct,
    // The samples after the post-filter and before the de-emphasis, the newest last.
    history: Vec<f32>,
    overlap: Vec<f32>,
    spectrum: Vec<f32>,
    shapes: Vec<f32>,
    norm: Vec<f32>,
    old_e: Energies,
    old_log_e: Energies,
    old_log_e2: Energies,
    background_log_e: Energies,
    postfilter: Pitch,
    postfilter_old: Pitch,
    preemph_mem: f32,
    rng: u32,
    loss_count: u32,
    start: usize,
    end: usize,
}

impl Decoder {
    pub(super) fn new() -> Decoder {
        let longest = SHORT << MAX_LM;
        let mut decoder = Decoder {
            mdct: Mdct::new(),
            history: vec![0.0; HISTORY],
            overlap: vec![0.0; OVERLAP],
            spectrum: vec![0.0; 2 * longest],
            shapes: vec![0.0; 2 * longest],
            norm: vec![0.0; 2 * (E_BANDS[BANDS] << MAX_LM)],
            old_e: [[0.0; BANDS]; 2],
            old_log_e: [[0.0; BANDS]; 2],
            old_log_e2: [[0.0; BANDS]; 2],
            background_log_e: [[0.0; BANDS]; 2],
            postfilter: Pitch::default(),
            postfilter_old: Pitch::default(),
            preemph_mem: 0.0,
            rng: 0,
            loss_count: 0,
            start: 0,
            end: BANDS,
        };
        decoder.reset();
        decoder
    }

    // The rising half of the window, over the 2.5 ms two frames overlap by.
    pub(super) fn window(&self) -> &[f32] {
        self.mdct.window()
    }

    // Forgets every frame decoded, as at a change of mode (4.5.2).
    pub(super) fn reset(&mut self) {
        self.history.fill(0.0);
        self.overlap.fill(0.0);
        self.norm.fill(0.0);
        self.old_e = [[0.0; BANDS]; 2];
        self.old_log_e = [[QUIET; BANDS]; 2];
        self.old_log_e2 = [[QUIET; BANDS]; 2];
        self.background_log_e = [[0.0; BANDS]; 2];
        self.postfilter = Pitch::default();
        self.postfilter_old = Pitch::default();
        self.preemph_mem = 0.0;
        self.rng = 0;
        self.loss_count = 0;
    }

    // Decodes a frame of `out.len()` samples, 120, 240, 480 or 960, of that bandwidth, of the bands above SILK's where it is hybrid, of one channel or two, from `range`, which SILK may have read from already; a frame of one byte or none is concealed. A frame that reads past its end is `Flaw::Broken`, its samples decoded all the same.
    pub(super) fn decode(
        &mut self,
        range: &mut RangeDecoder,
        bandwidth: Bandwidth,
        is_hybrid: bool,
        is_stereo: bool,
        out: &mut [f32],
    ) -> Result<(), Flaw> {
        let lm = lm_of(out.len())?;
        let start = if is_hybrid { HYBRID_START } else { 0 };
        let end = end_of(bandwidth);
        if start >= end {
            return Err(Flaw::Invalid("a hybrid frame of no band above SILK's"));
        }
        (self.start, self.end) = (start, end);
        if range.len() <= 1 {
            self.conceal_frame(lm, out);
            return Ok(());
        }
        let channels = if is_stereo { 2 } else { 1 };
        if channels == 1 {
            for band in 0..BANDS {
                self.old_e[0][band] = self.old_e[0][band].max(self.old_e[1][band]);
            }
        }
        let total_bits = (range.len() as u32 * u8::BITS) as i32;
        let is_silent = code_silence(range, false, total_bits);
        let pitch = if start == 0 { code_pitch(range, None, total_bits) } else { Pitch::default() };
        let is_transient = code_transient(range, lm, false, total_bits);
        let mut frame = Frame { start, end, lm, channels, is_transient, spread: SPREAD_NORMAL, tf_res: [0; BANDS] };
        energy::code_coarse(range, &frame, false, &mut self.old_e, |_, _, _, _, _| 0);
        frame.tf_res = bands::code_tf(range, &frame, &[0; BANDS], false);
        frame.spread = code_spread(range, SPREAD_NORMAL, total_bits);
        let cap = caps(lm, channels);
        let (offsets, total) = code_boosts(range, &frame, &cap, &[0; BANDS], total_bits);
        let alloc_trim = code_trim(range, TRIM_NONE, total);
        let (bits, anti_collapse_rsv) = shares(range, &frame, total_bits);
        let allocation = allocate(range, &frame, &offsets, &cap, alloc_trim, bits, &Choice::default());
        let mut errors = [[0.0; BANDS]; 2];
        energy::code_fine(range, &frame, &allocation, &mut self.old_e, &mut errors);
        let n = out.len();
        let m = 1 << lm;
        let shapes = &mut self.shapes[..channels * n];
        for channel in shapes.chunks_exact_mut(n) {
            channel[..m * E_BANDS[start]].fill(0.0);
            channel[m * E_BANDS[end]..].fill(0.0);
        }
        let norm = &mut self.norm[..2 * m * E_BANDS[BANDS]];
        let total = total_bits * BIT - anti_collapse_rsv;
        let collapse = bands::code(range, &frame, &allocation, total, shapes, norm, &mut self.rng);
        let is_anti_collapse = anti_collapse_rsv > 0 && range.bits(1) == 1;
        let left = total_bits - range.tell() as i32;
        energy::code_final(range, &frame, &allocation, left, &mut self.old_e, &mut errors);
        if is_anti_collapse {
            let energies = [&self.old_e, &self.old_log_e, &self.old_log_e2];
            bands::anti_collapse(&frame, shapes, &collapse, energies, &allocation.pulses, self.rng);
        }
        let mut amplitudes = energy::amplitudes(&self.old_e, start, end);
        if is_silent {
            amplitudes = [[0.0; BANDS]; 2];
            self.old_e[..channels].fill([QUIET; BANDS]);
        }
        let spectrum = &mut self.spectrum[..channels * n];
        for ((spectrum, shapes), amplitudes) in
            spectrum.chunks_exact_mut(n).zip(shapes.chunks_exact(n)).zip(&amplitudes)
        {
            bands::denormalise(shapes, amplitudes, lm, end, spectrum);
            spectrum[..m * E_BANDS[start]].fill(0.0);
        }
        if channels == 2 {
            let (left, right) = spectrum.split_at_mut(n);
            for (left, &right) in left.iter_mut().zip(right.iter()) {
                *left = 0.5 * (*left + right);
            }
        }
        self.synthesize(n, if is_transient { m } else { 1 });
        let at = HISTORY - n;
        let window = self.mdct.window();
        postfilter::filter(&mut self.history, at, SHORT, [self.postfilter_old, self.postfilter], window, false);
        if lm != 0 {
            postfilter::filter(&mut self.history, at + SHORT, n - SHORT, [self.postfilter, pitch], window, false);
        }
        self.postfilter_old = if lm != 0 { pitch } else { self.postfilter };
        self.postfilter = pitch;
        if channels == 1 {
            self.old_e[1] = self.old_e[0];
        }
        if is_transient {
            for (last, now) in self.old_log_e.as_flattened_mut().iter_mut().zip(self.old_e.as_flattened()) {
                *last = last.min(*now);
            }
        } else {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_e;
            let rise = m as f32 * BACKGROUND_RISE;
            for (background, now) in self.background_log_e.as_flattened_mut().iter_mut().zip(self.old_e.as_flattened())
            {
                *background = (*background + rise).min(*now);
            }
        }
        for channel in 0..2 {
            for band in (0..start).chain(end..BANDS) {
                self.old_e[channel][band] = 0.0;
                self.old_log_e[channel][band] = QUIET;
                self.old_log_e2[channel][band] = QUIET;
            }
        }
        self.rng = range.range();
        self.deemphasize(out);
        self.loss_count = 0;
        if range.tell() as i32 > total_bits { Err(Flaw::Broken) } else { Ok(()) }
    }

    // Conceals a lost frame of `out.len()` samples.
    pub(super) fn conceal(&mut self, out: &mut [f32]) -> Result<(), Flaw> {
        let lm = lm_of(out.len())?;
        self.conceal_frame(lm, out);
        Ok(())
    }

    // Noise in the last frame's bands at its energies less 9 dB, 3 dB less for each frame lost after, and from the fifth at the background's, overlapped with the frame before; the decoder's energies stay as they were (celt_decode_lost's noise).
    fn conceal_frame(&mut self, lm: usize, out: &mut [f32]) {
        let (start, end) = (self.start, self.end);
        let (n, m) = (out.len(), 1 << lm);
        let decay = FIRST_LOSS_DECAY + LOSS_DECAY * self.loss_count as f32;
        let level: Energies = core::array::from_fn(|channel| {
            core::array::from_fn(|band| {
                let last = self.old_log_e[channel][band];
                if self.loss_count >= BACKGROUND_LOSSES {
                    self.background_log_e[channel][band].min(last)
                } else {
                    last - decay
                }
            })
        });
        let [left, right] = energy::amplitudes(&level, start, end);
        let amplitudes: [f32; BANDS] = core::array::from_fn(|band| left[band].max(right[band]));
        let shapes = &mut self.shapes[..n];
        shapes.fill(0.0);
        for band in start..end {
            let bins = &mut shapes[m * E_BANDS[band]..m * E_BANDS[band + 1]];
            bands::noise(bins, &mut self.rng);
            renormalise(bins, 1.0);
        }
        bands::denormalise(shapes, &amplitudes, lm, end, &mut self.spectrum[..n]);
        self.synthesize(n, 1);
        self.deemphasize(out);
        self.loss_count = self.loss_count.saturating_add(1);
    }

    // The history moved on by `n` samples and the inverse MDCT of the spectrum's first channel after it.
    fn synthesize(&mut self, n: usize, blocks: usize) {
        self.history.copy_within(n.., 0);
        self.mdct.inverse(&self.spectrum[..n], blocks, &mut self.history[HISTORY - n..], &mut self.overlap);
    }

    // The newest samples of the history through the de-emphasis into `out` (4.3.7.2).
    fn deemphasize(&mut self, out: &mut [f32]) {
        let n = out.len();
        for (out, &sample) in out.iter_mut().zip(&self.history[HISTORY - n..]) {
            let value = sample + self.preemph_mem;
            self.preemph_mem = PREEMPHASIS * value;
            *out = value;
        }
    }
}
