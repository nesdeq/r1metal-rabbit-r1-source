// Opus (RFC 6716): packets decoded into one channel at 48 kHz, by the SILK and the CELT layer over the range coder they share; packets of CELT frames encoded from one channel at a constant rate.

mod celt;
mod encode;
mod packet;
mod range;
mod silk;
#[cfg(test)]
pub(crate) mod support;
#[cfg(test)]
mod tests;

pub(crate) use encode::Encoder;
pub(crate) use packet::Bandwidth;

use crate::base::math::to_sample;
use crate::base::resample::Resampler;
use crate::codec::Flaw;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use packet::{Mode, Packet, Toc};
use range::RangeDecoder;

pub(crate) const RATE: u32 = packet::RATE;
// 2.5 ms: the samples one sound fades into another over, by the CELT layer's window (4.5.1.4).
const FADE: usize = 120;
// 5 ms: a redundant CELT frame (4.5.1), and what a change of mode without one is bridged by.
const BRIDGE: usize = 2 * FADE;
// 20 ms: the most that is concealed at a time; and 10 ms, the least the SILK layer conceals.
const MOST_CONCEALED: usize = 960;
const LEAST_SILK: usize = 480;
// A frame's SILK layer at its longest: 60 ms at 16 kHz.
const MOST_SILK: usize = 960;
// The bits a frame has left behind its SILK layer where a redundant CELT frame may follow, the more a hybrid frame's flag and size take, the flag's logp, the values of the size and the least size (4.5.1).
const REDUNDANCY_ROOM: u32 = 17;
const HYBRID_ROOM: u32 = 20;
const REDUNDANCY_LOGP: u32 = 12;
const REDUNDANCY_SIZES: u32 = 256;
const LEAST_REDUNDANCY: usize = 2;
// A CELT frame of silence, which fades the CELT layer out where a hybrid frame is followed by one of SILK alone (4.5.2).
const SILENCE: [u8; 2] = [0xFF, 0xFF];
// The SILK layer's rates by its bandwidth (4.2), and the samples at 48 kHz its sound lies behind the CELT layer's: Table 54's delays of 0.538, 0.692 and 0.706 ms, which count from the SILK decoder's samples, as libopus's sound against this decoder's shows (27, 34 and 34 samples measured).
const SILK_RATES: [u32; 3] = [8000, 12_000, 16_000];
const SILK_LATE: [u64; 3] = [26, 33, 34];

// A stretch of the decoded sound that another sound is spliced into: `lead` samples of the first sound alone, then a fade into the second. The first is `other` and the second the decoded sound, or the other way round where it ends in `other`.
struct Splice {
    at: u64,
    lead: usize,
    other: [f32; BRIDGE],
    ends_in_other: bool,
}

// The SILK layer's sound on its way to 48 kHz: its bandwidth's place in `SILK_RATES`, and the time of the next sample that comes of it.
struct Stream {
    band: usize,
    at: u64,
}

// A decoder of Opus packets into one channel at 48 kHz. Times count samples from the first decoded; what is given lies `late` samples behind them, the time the filters that bring SILK's rates to 48 kHz hold their samples back.
pub(crate) struct Decoder {
    silk: silk::Decoder,
    celt: celt::Decoder,
    upsamplers: [Resampler; 3],
    late: u64,
    // The mode of the last frame, whether it ended in a redundant CELT frame, and the mode of the last packet.
    last: Option<Mode>,
    is_bridged: bool,
    packet_mode: Option<Mode>,
    // The samples decoded and the samples given; the sound from the first sample not given yet on, both layers summed.
    now: u64,
    given: u64,
    line: VecDeque<f32>,
    stream: Option<Stream>,
    splices: Vec<Splice>,
    low: Vec<i16>,
    risen: Vec<i16>,
    high: Vec<f32>,
    final_range: u32,
}

// The samples concealed next of `left` lost: 20 ms, 10 ms, or what is left of less; the CELT layer conceals 5 ms of more than that (opus_decode_frame).
const fn concealed(left: usize, mode: Mode) -> usize {
    match left {
        MOST_CONCEALED.. => MOST_CONCEALED,
        LEAST_SILK.. => LEAST_SILK,
        _ if !matches!(mode, Mode::Silk) && left > BRIDGE => BRIDGE,
        _ => left,
    }
}

impl Decoder {
    pub(crate) fn new() -> Decoder {
        let upsamplers = SILK_RATES.map(|rate| Resampler::new(rate, RATE));
        let late = upsamplers.iter().zip(SILK_LATE).map(|(filter, late)| filter.held().saturating_sub(late)).max();
        Decoder {
            silk: silk::Decoder::new(),
            celt: celt::Decoder::new(),
            upsamplers,
            late: late.unwrap_or(0),
            last: None,
            is_bridged: false,
            packet_mode: None,
            now: 0,
            given: 0,
            line: VecDeque::new(),
            stream: None,
            splices: Vec::new(),
            low: vec![0; MOST_SILK],
            risen: Vec::new(),
            high: vec![0.0; MOST_CONCEALED],
            final_range: 0,
        }
    }

    // Forgets every packet decoded, as a new stream begins, and keeps the filters' coefficients (libopus's `OPUS_RESET_STATE`).
    pub(crate) fn reset(&mut self) {
        self.silk.reset();
        self.celt.reset();
        self.upsamplers.iter_mut().for_each(|filter| _ = filter.seek(0));
        (self.last, self.is_bridged, self.packet_mode, self.stream) = (None, false, None, None);
        (self.now, self.given, self.final_range) = (0, 0, 0);
        self.line.clear();
        self.splices.clear();
    }

    // The samples a packet lasts.
    pub(crate) fn samples_of(packet: &[u8]) -> Result<usize, Flaw> {
        Packet::read(packet).map(|packet| packet.samples())
    }

    // The samples before a packet that the copy it holds of them covers, its first frame's (4.2.5); none where it holds none, as a CELT frame never does.
    pub(crate) fn copy_of(packet: &[u8]) -> Option<usize> {
        let packet = Packet::read(packet).ok()?;
        let (toc, first) = (packet.toc, packet.frames()[0]);
        let is_copied = toc.mode != Mode::Celt
            && first.len() > 1
            && silk::has_lbrr(&mut RangeDecoder::new(first), toc.frame, toc.is_stereo);
        is_copied.then_some(toc.frame)
    }

    #[cfg(test)]
    const fn final_range(&self) -> u32 {
        self.final_range
    }

    // Decodes a packet into `out` and gives the samples written; one that does not parse changes nothing and is for the caller to conceal.
    pub(crate) fn decode(&mut self, packet: &[u8], out: &mut [i16]) -> Result<usize, Flaw> {
        let packet = Packet::read(packet)?;
        let out = out.get_mut(..packet.samples()).ok_or(Flaw::Short)?;
        for frame in packet.frames() {
            self.frame(frame, packet.toc, false);
        }
        self.packet_mode = Some(packet.toc.mode);
        self.give(out);
        Ok(out.len())
    }

    // Makes up for a lost packet of `samples` samples from the packet behind it: its last frame's time from that packet's copy of it where it holds one (4.2.5), concealment before.
    pub(crate) fn recover(&mut self, next: &[u8], samples: usize, out: &mut [i16]) -> Result<usize, Flaw> {
        let packet = Packet::read(next)?;
        let (toc, copy) = (packet.toc, packet.frames()[0]);
        if samples < toc.frame || toc.mode == Mode::Celt || self.packet_mode == Some(Mode::Celt) {
            return self.conceal(samples, out);
        }
        let out = out.get_mut(..samples).ok_or(Flaw::Short)?;
        self.conceal_span(samples - toc.frame)?;
        self.frame(copy, toc, true);
        self.packet_mode = Some(toc.mode);
        self.give(out);
        Ok(samples)
    }

    // Conceals `samples` lost samples, a count of whole 2.5 ms.
    pub(crate) fn conceal(&mut self, samples: usize, out: &mut [i16]) -> Result<usize, Flaw> {
        let out = out.get_mut(..samples).ok_or(Flaw::Short)?;
        self.conceal_span(samples)?;
        self.give(out);
        Ok(samples)
    }

    fn conceal_span(&mut self, samples: usize) -> Result<(), Flaw> {
        if !samples.is_multiple_of(FADE) {
            return Err(Flaw::Invalid("a time to conceal that is no count of 2.5 ms"));
        }
        self.conceal_parts(samples);
        Ok(())
    }

    // Conceals `samples` in the parts concealed at a time; with nothing decoded yet a part only passes its time, whatever its length.
    fn conceal_parts(&mut self, samples: usize) {
        let mut left = samples;
        while left > 0 {
            let part = concealed(left, self.concealing().unwrap_or(Mode::Celt));
            self.conceal_frame(part);
            left -= part;
        }
    }

    // The mode a loss is concealed in: the last frame's, or CELT's where that ended in a redundant CELT frame.
    fn concealing(&self) -> Option<Mode> {
        if self.is_bridged { Some(Mode::Celt) } else { self.last }
    }

    fn conceal_frame(&mut self, samples: usize) {
        let start = self.now;
        self.now += samples as u64;
        self.final_range = 0;
        let Some(mode) = self.concealing() else {
            return;
        };
        if mode == Mode::Celt {
            self.end_stream(start);
        } else if let Some(bandwidth) = self.silk.bandwidth() {
            // The SILK layer conceals 10 ms at the least, of which the first part is taken.
            let frame = samples.max(LEAST_SILK);
            let count = self.silk.conceal(frame, &mut self.low).unwrap_or(0);
            self.feed(bandwidth as usize, start, count * samples / frame);
        }
        if mode != Mode::Silk {
            let mut high = core::mem::take(&mut self.high);
            if self.celt.conceal(&mut high[..samples]).is_ok() {
                self.lay(start, &high[..samples]);
            }
            self.high = high;
        }
        (self.last, self.is_bridged) = (Some(mode), false);
    }

    // Decodes a frame of a packet, or, of the packet behind a lost one, its copy of that one's last frame.
    fn frame(&mut self, bytes: &[u8], toc: Toc, is_copy: bool) {
        let Toc { mode, bandwidth, frame: samples, is_stereo } = toc;
        if bytes.len() <= 1 {
            // A frame that was not sent is concealed, for no longer than it lasts.
            self.conceal_parts(samples);
            return;
        }
        let start = self.now;
        let bridge = samples.min(BRIDGE);
        let is_change = self.last.is_some_and(|last| (last == Mode::Celt) != (mode == Mode::Celt));
        let mut is_unbridged = is_change && !(mode == Mode::Celt && self.is_bridged);
        let before = self.last;
        if is_unbridged && mode == Mode::Celt {
            // The layers of the mode before conceal the frame's beginning, which then fades into it.
            self.conceal_frame(bridge);
            (self.now, self.last) = (start, before);
        }
        if mode == Mode::Celt {
            self.end_stream(start + if is_unbridged { bridge as u64 } else { 0 });
        }
        let mut range = RangeDecoder::new(bytes);
        if mode != Mode::Celt {
            if before == Some(Mode::Celt) {
                self.silk.reset();
            }
            let inner = if mode == Mode::Hybrid { Bandwidth::Wide } else { bandwidth };
            let decoded = if is_copy {
                self.silk.decode_lbrr(&mut range, inner, samples, is_stereo, &mut self.low)
            } else {
                self.silk.decode(&mut range, inner, samples, is_stereo, &mut self.low)
            };
            self.feed(inner as usize, start, decoded.unwrap_or(0));
        }
        let mut length = bytes.len();
        let mut redundant: Option<(&[u8], bool)> = None;
        let room = REDUNDANCY_ROOM + if mode == Mode::Hybrid { HYBRID_ROOM } else { 0 };
        if !is_copy
            && mode != Mode::Celt
            && range.tell() + room <= length as u32 * u8::BITS
            && (mode != Mode::Hybrid || range.bit(REDUNDANCY_LOGP))
        {
            let is_first = range.bit(1);
            let size = if mode == Mode::Hybrid {
                range.uint(REDUNDANCY_SIZES) as usize + LEAST_REDUNDANCY
            } else {
                length - range.tell().div_ceil(u8::BITS) as usize
            };
            match length.checked_sub(size).filter(|left| *left as u32 * u8::BITS >= range.tell()) {
                Some(left) => {
                    range.shorten(size);
                    redundant = Some((&bytes[left..], is_first));
                    length = left;
                }
                // A size past the frame leaves no frame at all (opus_decode_frame).
                None => {
                    range.shorten(length);
                    length = 0;
                }
            }
        }
        let is_bridged_first = redundant.is_some_and(|(_, is_first)| is_first);
        is_unbridged &= redundant.is_none();
        if is_unbridged && mode != Mode::Celt {
            // The CELT layer conceals the frame's beginning, which then fades into it.
            let mut other = [0.0; BRIDGE];
            if self.celt.conceal(&mut other[..bridge]).is_ok() {
                self.splices.push(Splice { at: start, lead: bridge - FADE, other, ends_in_other: false });
            }
        }
        let (mut sound, mut redundant_range) = ([0.0; BRIDGE], 0);
        if let Some((frame, true)) = redundant {
            let mut coder = RangeDecoder::new(frame);
            let _ = self.celt.decode(&mut coder, bandwidth, false, is_stereo, &mut sound);
            redundant_range = coder.range();
        }
        let mut high = core::mem::take(&mut self.high);
        let mut high_samples = 0;
        if mode != Mode::Silk {
            if before.is_some_and(|last| last != mode) && !self.is_bridged {
                self.celt.reset();
            }
            high_samples = samples.min(MOST_CONCEALED);
            let part = &mut high[..high_samples];
            // A frame the CELT layer reads past its end has its samples all the same.
            let _ = if is_copy {
                self.celt.conceal(part)
            } else {
                self.celt.decode(&mut range, bandwidth, mode == Mode::Hybrid, is_stereo, part)
            };
        } else if before == Some(Mode::Hybrid) && !(is_bridged_first && self.is_bridged) {
            high_samples = FADE;
            let _ = self.celt.decode(&mut RangeDecoder::new(&SILENCE), bandwidth, false, is_stereo, &mut high[..FADE]);
        }
        if is_unbridged && mode == Mode::Celt {
            let mut other = [0.0; BRIDGE];
            other[..bridge].copy_from_slice(&high[..bridge]);
            self.splices.push(Splice { at: start, lead: bridge - FADE, other, ends_in_other: true });
            self.lay(start + bridge as u64, &high[bridge..high_samples]);
        } else {
            self.lay(start, &high[..high_samples]);
        }
        self.high = high;
        match redundant {
            Some((frame, false)) => {
                self.celt.reset();
                let mut coder = RangeDecoder::new(frame);
                let _ = self.celt.decode(&mut coder, bandwidth, false, is_stereo, &mut sound);
                redundant_range = coder.range();
                let mut other = [0.0; BRIDGE];
                other[..FADE].copy_from_slice(&sound[FADE..]);
                let at = start + (samples - FADE) as u64;
                self.splices.push(Splice { at, lead: 0, other, ends_in_other: true });
            }
            // A redundant frame that follows no CELT frame, the one before it lost, is of no use (opus_decode_frame).
            Some((_, true)) if before != Some(Mode::Silk) || self.is_bridged => {
                self.splices.push(Splice { at: start, lead: FADE, other: sound, ends_in_other: false });
            }
            _ => {}
        }
        self.final_range = if length <= 1 { 0 } else { range.range() ^ redundant_range };
        (self.last, self.is_bridged) = (Some(mode), redundant.is_some_and(|(_, is_first)| !is_first));
        self.now = start + samples as u64;
    }

    // Takes `count` samples of the SILK layer, of the time from `at` on, into the stream of their rate, a new one where the rate changed.
    fn feed(&mut self, band: usize, at: u64, count: usize) {
        if self.stream.as_ref().is_none_or(|stream| stream.band != band) {
            self.end_stream(at);
            self.upsamplers[band].seek(0);
            self.stream = Some(Stream { band, at: at + SILK_LATE[band] });
        }
        let mut risen = core::mem::take(&mut self.risen);
        risen.clear();
        for &sample in &self.low[..count] {
            self.upsamplers[band].push(sample, &mut risen);
        }
        self.lay_risen(&risen, u64::MAX);
        self.risen = risen;
    }

    // Ends the SILK layer's stream: its last samples come as before silence, and none of them from `cut` on.
    fn end_stream(&mut self, cut: u64) {
        let Some(band) = self.stream.as_ref().map(|stream| stream.band) else {
            return;
        };
        let mut risen = core::mem::take(&mut self.risen);
        risen.clear();
        self.upsamplers[band].finish(&mut risen);
        self.lay_risen(&risen, cut);
        self.risen = risen;
        self.stream = None;
    }

    fn lay_risen(&mut self, risen: &[i16], cut: u64) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        let at = stream.at;
        stream.at += risen.len() as u64;
        let kept = cut.saturating_sub(at).min(risen.len() as u64) as usize;
        self.add(at, risen[..kept].iter().map(|&sample| f32::from(sample)));
    }

    fn lay(&mut self, at: u64, samples: &[f32]) {
        self.add(at, samples.iter().copied());
    }

    // Adds samples of the time from `at` on to the sound that waits to be given; what lies before it was given.
    fn add(&mut self, at: u64, samples: impl ExactSizeIterator<Item = f32>) {
        let place = (at + self.late) as i64 - self.given as i64;
        let end = usize::try_from(place + samples.len() as i64).unwrap_or(0);
        if self.line.len() < end {
            self.line.resize(end, 0.0);
        }
        for (step, sample) in samples.enumerate() {
            if let Some(slot) = usize::try_from(place + step as i64).ok().and_then(|at| self.line.get_mut(at)) {
                *slot += sample;
            }
        }
    }

    // Gives the next samples of the sound, spliced where it is to be.
    fn give(&mut self, out: &mut [i16]) {
        if self.line.len() < out.len() {
            self.line.resize(out.len(), 0.0);
        }
        let window = self.celt.window();
        for splice in &self.splices {
            for step in 0..splice.lead + FADE {
                let place = (splice.at + step as u64 + self.late).checked_sub(self.given);
                let Some(slot) =
                    place.filter(|at| *at < out.len() as u64).and_then(|at| self.line.get_mut(at as usize))
                else {
                    continue;
                };
                let (first, second) =
                    if splice.ends_in_other { (*slot, splice.other[step]) } else { (splice.other[step], *slot) };
                *slot = match step.checked_sub(splice.lead) {
                    None => first,
                    Some(faded) => {
                        let weight = window[faded] * window[faded];
                        weight * second + (1.0 - weight) * first
                    }
                };
            }
        }
        let count = out.len();
        for (sample, sound) in out.iter_mut().zip(self.line.drain(..count)) {
            *sample = to_sample(sound);
        }
        self.given += count as u64;
        let (given, late) = (self.given, self.late);
        self.splices.retain(|splice| splice.at + (splice.lead + FADE) as u64 + late > given);
    }
}
