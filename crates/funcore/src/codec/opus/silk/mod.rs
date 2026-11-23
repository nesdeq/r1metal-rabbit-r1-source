use super::packet::{Bandwidth, SILK_FRAMES};
use super::range::RangeDecoder;
use crate::codec::Flaw;
use indices::Signal;

mod conceal;
mod fixed;
mod indices;
mod lpc;
mod params;
mod pulses;
mod synthesis;
mod tables;
#[cfg(test)]
mod tests;

const SUBFRAME_MS: usize = 5;
const MOST_SUBFRAMES: usize = 4;
const MOST_FRAMES: usize = 3;
// The internal rates of NB, MB and WB in kHz (4.2).
const KHZ: [usize; 3] = [8, 12, 16];
const NARROW: usize = 0;
const WIDE: usize = 2;
const MOST_SUBFRAME: usize = SUBFRAME_MS * KHZ[WIDE];
const MOST_FRAME: usize = MOST_SUBFRAMES * MOST_SUBFRAME;
const MOST_ORDER: usize = 16;
const NARROW_ORDER: usize = 10;
// The output of the frames before that rewhitening reads (4.2.7.9.1), and the room it is kept in.
const HISTORY_MS: usize = 20;
const HISTORY: usize = MOST_FRAME + 2 * MOST_SUBFRAME;
// A pitch lag lies from 2 ms to 18 ms (4.2.7.6.1).
const LEAST_LAG_MS: usize = 2;
const MOST_LAG_MS: usize = 18;
// The SILK frames of each Opus frame duration of SILK_FRAMES, and the subframes of each (4.2.2).
const LAYOUTS: [(usize, usize); 4] = [(1, 2), (1, 4), (2, 4), (3, 4)];
// What a change of rate leaves of the last pitch lag and gain index (silk/decoder_set_fs.c); 4.2.7.4's clamp changes no gain index after 10.
const RESET_LAG: i32 = 100;
const RESET_GAIN_INDEX: i32 = 10;
const UNIT_GAIN_Q16: i32 = 1 << 16;

// The internal rate and the subframes of the SILK frames of an Opus frame.
#[derive(Clone, Copy)]
struct Layout {
    band: usize,
    subframes: usize,
}

impl Layout {
    const fn khz(self) -> usize {
        KHZ[self.band]
    }

    const fn subframe(self) -> usize {
        SUBFRAME_MS * self.khz()
    }

    const fn frame(self) -> usize {
        self.subframes * self.subframe()
    }

    const fn is_wide(self) -> bool {
        self.band == WIDE
    }

    const fn order(self) -> usize {
        if self.is_wide() { MOST_ORDER } else { NARROW_ORDER }
    }

    const fn history(self) -> usize {
        HISTORY_MS * self.khz()
    }

    const fn lags(self) -> (i32, i32) {
        ((LEAST_LAG_MS * self.khz()) as i32, (MOST_LAG_MS * self.khz()) as i32)
    }
}

// The SILK frames and the layout of an Opus frame of that many samples at 48 kHz.
fn layout(band: usize, frame: usize) -> Result<(usize, Layout), Flaw> {
    let at = SILK_FRAMES.iter().position(|&samples| samples == frame).ok_or(Flaw::Invalid("a duration of SILK"))?;
    let (frames, subframes) = LAYOUTS[at];
    Ok((frames, Layout { band, subframes }))
}

// How a frame's first gain, its lag and its LTP scaling are coded (4.2.7.4, 4.2.7.6.1, 4.2.7.6.3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Condition {
    Independent,
    IndependentUnscaled,
    Conditional,
}

// The header bits of a channel (4.2.3, 4.2.4).
#[derive(Clone, Copy, Default)]
struct Flags {
    vad: [bool; MOST_FRAMES],
    lbrr: [bool; MOST_FRAMES],
}

// What reading a channel's frame takes from the frame before it of the same kind.
#[derive(Clone, Copy, Default)]
struct Parse {
    flags: Flags,
    signal: Signal,
    lag: i32,
}

// A channel that is synthesized: what its next frame takes of the frames before.
struct Channel {
    parse: Parse,
    prev_gain: i32,
    excitation: [i32; MOST_FRAME],
    lpc: [i32; MOST_ORDER],
    history: [i16; HISTORY],
    lag: i32,
    gain_index: i32,
    nlsf: [i16; MOST_ORDER],
    is_reset: bool,
    losses: i32,
    signal: Signal,
    plc: conceal::Plc,
}

impl Channel {
    fn new() -> Channel {
        Channel {
            parse: Parse::default(),
            prev_gain: UNIT_GAIN_Q16,
            excitation: [0; MOST_FRAME],
            lpc: [0; MOST_ORDER],
            history: [0; HISTORY],
            lag: 0,
            gain_index: 0,
            nlsf: [0; MOST_ORDER],
            is_reset: true,
            losses: 0,
            signal: Signal::Inactive,
            plc: conceal::Plc::new(),
        }
    }

    // What a change of the internal rate clears.
    fn renew(&mut self) {
        self.is_reset = true;
        self.lag = RESET_LAG;
        self.gain_index = RESET_GAIN_INDEX;
        self.signal = Signal::Inactive;
        self.history = [0; HISTORY];
        self.lpc = [0; MOST_ORDER];
    }

    fn decode(
        &mut self,
        range: &mut RangeDecoder,
        layout: Layout,
        index: usize,
        is_lbrr: bool,
        condition: Condition,
        out: &mut [i16],
    ) {
        let indices = indices::read(range, &mut self.parse, layout, index, is_lbrr, condition);
        let pulses = pulses::read(range, indices.signal, indices.offset, layout.frame());
        let mut params = params::decode(self, layout, &indices, condition);
        synthesis::synthesize(self, layout, &indices, &mut params, &pulses, out);
        conceal::update(self, layout, &indices, &params);
        self.losses = 0;
        self.signal = indices.signal;
        self.is_reset = false;
        self.finish(layout, params.pitch[layout.subframes - 1], out);
    }

    fn conceal(&mut self, layout: Layout, out: &mut [i16]) {
        let lag = conceal::conceal(self, layout, out);
        self.losses = self.losses.saturating_add(1);
        self.finish(layout, lag, out);
    }

    fn finish(&mut self, layout: Layout, lag: i32, out: &mut [i16]) {
        let (history, frame) = (layout.history(), layout.frame());
        self.history.copy_within(frame..history, 0);
        self.history[history - frame..history].copy_from_slice(out);
        conceal::glue(self, out);
        self.lag = lag;
    }
}

// The decoder of the SILK layer for one channel of output: a stereo frame gives its mid channel, one sample late as the stereo unmixing of 4.2.8 delays it, and so does a mono frame. Samples are at the internal rate.
pub(super) struct Decoder {
    layout: Option<Layout>,
    mid: Channel,
    side: Parse,
    is_stereo: bool,
    was_mid_only: bool,
    held: i16,
}

impl Decoder {
    pub(super) fn new() -> Decoder {
        Decoder {
            layout: None,
            mid: Channel::new(),
            side: Parse::default(),
            is_stereo: false,
            was_mid_only: false,
            held: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Decoder::new();
    }

    // The internal bandwidth of the last frame decoded since the decoder was made or reset.
    pub(super) fn bandwidth(&self) -> Option<Bandwidth> {
        self.layout.map(|layout| [Bandwidth::Narrow, Bandwidth::Medium, Bandwidth::Wide][layout.band])
    }

    // Decodes the regular SILK frames of an Opus frame of `frame` samples at 48 kHz into `out` and gives the samples written; the range decoder is left where the SILK layer ends.
    pub(super) fn decode(
        &mut self,
        range: &mut RangeDecoder,
        bandwidth: Bandwidth,
        frame: usize,
        is_stereo: bool,
        out: &mut [i16],
    ) -> Result<usize, Flaw> {
        self.read(range, bandwidth, frame, is_stereo, false, out)
    }

    // Decodes the LBRR frames an Opus frame holds of the one before it, concealing each SILK frame they hold none of.
    pub(super) fn decode_lbrr(
        &mut self,
        range: &mut RangeDecoder,
        bandwidth: Bandwidth,
        frame: usize,
        is_stereo: bool,
        out: &mut [i16],
    ) -> Result<usize, Flaw> {
        self.read(range, bandwidth, frame, is_stereo, true, out)
    }

    // Conceals a lost Opus frame of `frame` samples at 48 kHz at the internal rate of the last frame; with none decoded yet it writes nothing.
    pub(super) fn conceal(&mut self, frame: usize, out: &mut [i16]) -> Result<usize, Flaw> {
        let Some(last) = self.layout else {
            return Ok(0);
        };
        let (frames, layout) = layout(last.band, frame)?;
        let out = out.get_mut(..frames * layout.frame()).ok_or(Flaw::Short)?;
        self.set_layout(layout);
        for part in out.chunks_exact_mut(layout.frame()) {
            let mut samples = [0; MOST_FRAME];
            let samples = &mut samples[..layout.frame()];
            self.mid.conceal(layout, samples);
            // 4.2.7.4 lets the clamp of the next independent gain be skipped after a loss, as the reference does.
            self.mid.gain_index = RESET_GAIN_INDEX;
            self.hold(samples, part);
        }
        Ok(out.len())
    }

    fn read(
        &mut self,
        range: &mut RangeDecoder,
        bandwidth: Bandwidth,
        frame: usize,
        is_stereo: bool,
        is_lbrr: bool,
        out: &mut [i16],
    ) -> Result<usize, Flaw> {
        let band = bandwidth as usize;
        if band >= KHZ.len() {
            return Err(Flaw::Invalid("a bandwidth of SILK"));
        }
        let (frames, layout) = layout(band, frame)?;
        let out = out.get_mut(..frames * layout.frame()).ok_or(Flaw::Short)?;
        if is_stereo && !self.is_stereo {
            self.side = Parse::default();
        }
        self.is_stereo = is_stereo;
        self.set_layout(layout);
        let mid_lbrr = read_flags(range, &mut self.mid.parse.flags, frames);
        let side_lbrr = is_stereo && read_flags(range, &mut self.side.flags, frames);
        read_lbrr_flags(range, &mut self.mid.parse.flags, frames, mid_lbrr);
        read_lbrr_flags(range, &mut self.side.flags, frames, side_lbrr);
        if !is_lbrr {
            self.skip_lbrr(range, layout, frames);
        }
        for (index, part) in out.chunks_exact_mut(layout.frame()).enumerate() {
            self.frame(range, layout, index, is_lbrr, part);
        }
        Ok(out.len())
    }

    fn set_layout(&mut self, layout: Layout) {
        if self.layout.map(|last| last.band) != Some(layout.band) {
            self.mid.renew();
        }
        self.layout = Some(layout);
    }

    // Reads past the LBRR frames that stand before the regular ones (4.2.5).
    fn skip_lbrr(&mut self, range: &mut RangeDecoder, layout: Layout, frames: usize) {
        for index in 0..frames {
            let (mid, side) = (self.mid.parse.flags.lbrr, self.side.flags.lbrr);
            if mid[index] {
                if self.is_stereo {
                    indices::skip_weights(range);
                    if !side[index] {
                        indices::read_mid_only(range);
                    }
                }
                skip_frame(range, &mut self.mid.parse, layout, index, true, after_lbrr(&mid, index));
            }
            if self.is_stereo && side[index] {
                skip_frame(range, &mut self.side, layout, index, true, after_lbrr(&side, index));
            }
        }
    }

    // Decodes the SILK frame of that number in an Opus frame, of the mid channel where the frame is stereo.
    fn frame(&mut self, range: &mut RangeDecoder, layout: Layout, index: usize, is_lbrr: bool, out: &mut [i16]) {
        let (mid, side) = (self.mid.parse.flags, self.side.flags);
        let is_mid_coded = !is_lbrr || mid.lbrr[index];
        let mut is_mid_only = false;
        if self.is_stereo && is_mid_coded {
            indices::skip_weights(range);
            let is_side_coded = if is_lbrr { side.lbrr[index] } else { side.vad[index] };
            if !is_side_coded {
                is_mid_only = indices::read_mid_only(range);
            }
        }
        let mut samples = [0; MOST_FRAME];
        let samples = &mut samples[..layout.frame()];
        if is_mid_coded {
            let condition = if is_lbrr { after_lbrr(&mid.lbrr, index) } else { after_regular(index) };
            self.mid.decode(range, layout, index, is_lbrr, condition, samples);
        } else {
            self.mid.conceal(layout, samples);
        }
        let is_side_coded = if is_lbrr { side.lbrr[index] } else { !is_mid_only };
        if self.is_stereo && is_side_coded {
            let condition = match (is_lbrr, self.was_mid_only) {
                (true, _) => after_lbrr(&side.lbrr, index),
                (false, true) if index > 0 => Condition::IndependentUnscaled,
                _ => after_regular(index),
            };
            skip_frame(range, &mut self.side, layout, index, is_lbrr, condition);
        }
        self.was_mid_only = is_mid_only;
        self.hold(samples, out);
    }

    // Gives a frame one sample late, the delay 4.2.8 has mono frames keep as stereo ones do.
    fn hold(&mut self, frame: &[i16], out: &mut [i16]) {
        let last = frame.len() - 1;
        out[0] = self.held;
        out[1..].copy_from_slice(&frame[..last]);
        self.held = frame[last];
    }
}

// Whether an Opus frame of `frame` samples at 48 kHz holds LBRR frames of the one before it: a channel's LBRR flag, read behind its VAD flags (4.2.3).
pub(super) fn has_lbrr(range: &mut RangeDecoder, frame: usize, is_stereo: bool) -> bool {
    let Ok((frames, _)) = layout(WIDE, frame) else {
        return false;
    };
    let mut flags = Flags::default();
    let mid = read_flags(range, &mut flags, frames);
    let side = is_stereo && read_flags(range, &mut flags, frames);
    mid || side
}

// Reads a channel's VAD flags and gives its LBRR flag (4.2.3).
fn read_flags(range: &mut RangeDecoder, flags: &mut Flags, frames: usize) -> bool {
    for vad in &mut flags.vad[..frames] {
        *vad = range.bit(1);
    }
    range.bit(1)
}

// Reads a channel's per-frame LBRR flags, each frame's from the lowest bit up; an Opus frame of one SILK frame has its flag in the header (4.2.4).
fn read_lbrr_flags(range: &mut RangeDecoder, flags: &mut Flags, frames: usize, has_lbrr: bool) {
    flags.lbrr = [false; MOST_FRAMES];
    if !has_lbrr {
        return;
    }
    let symbol = match frames {
        1 => 1,
        2 => range.icdf(&tables::LBRR_FLAGS_TWO, tables::PDF_BITS) + tables::LBRR_FLAGS_OFFSET,
        _ => range.icdf(&tables::LBRR_FLAGS_THREE, tables::PDF_BITS) + tables::LBRR_FLAGS_OFFSET,
    };
    for (frame, lbrr) in flags.lbrr[..frames].iter_mut().enumerate() {
        *lbrr = symbol >> frame & 1 != 0;
    }
}

// Reads a frame that is not synthesized: a side channel's, or an LBRR frame passed over.
fn skip_frame(
    range: &mut RangeDecoder,
    parse: &mut Parse,
    layout: Layout,
    index: usize,
    is_lbrr: bool,
    condition: Condition,
) {
    let indices = indices::read(range, parse, layout, index, is_lbrr, condition);
    pulses::read(range, indices.signal, indices.offset, layout.frame());
}

const fn after_regular(index: usize) -> Condition {
    if index == 0 { Condition::Independent } else { Condition::Conditional }
}

const fn after_lbrr(lbrr: &[bool; MOST_FRAMES], index: usize) -> Condition {
    if index > 0 && lbrr[index - 1] { Condition::Conditional } else { Condition::Independent }
}
