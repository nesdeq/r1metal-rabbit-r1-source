use super::celt::{self, FRAME};
use super::packet::{self, Bandwidth, Mode, RATE, Toc};
use crate::codec::Flaw;

// The bytes a CELT frame takes at the least (celt_encode_with_ec).
const LEAST_FRAME: usize = 2;
// The high-pass filter that takes a constant level out of the sound before it is coded: its cutoff, and its pole's step, 6.3 times the cutoff over the rate (dc_reject).
const CUTOFF_HZ: f32 = 3.0;
const POLE_STEP: f32 = 6.3 * CUTOFF_HZ / RATE as f32;

// An encoder of Opus packets of CELT frames of 20 ms from one channel at 48 kHz, each packet the size it is given (libopus in its restricted low-delay application at a constant rate).
pub(crate) struct Encoder {
    celt: celt::Encoder,
    toc: Toc,
    level: f32,
    final_range: u32,
}

impl Encoder {
    pub(crate) fn new(bandwidth: Bandwidth) -> Encoder {
        // CELT has no medium band: such sound is coded as wideband, as libopus does.
        let bandwidth = if bandwidth == Bandwidth::Medium { Bandwidth::Wide } else { bandwidth };
        Encoder {
            celt: celt::Encoder::new(bandwidth),
            toc: Toc { mode: Mode::Celt, bandwidth, frame: FRAME, is_stereo: false },
            level: 0.0,
            final_range: 0,
        }
    }

    #[cfg(test)]
    pub(super) const fn final_range(&self) -> u32 {
        self.final_range
    }

    // Encodes `pcm`, 1 to 6 frames of 20 ms, into one packet that fills `packet`, its frames all of one size, the largest that fits; the bytes written.
    pub(crate) fn encode(&mut self, pcm: &[i16], packet: &mut [u8]) -> Result<usize, Flaw> {
        if !pcm.len().is_multiple_of(FRAME) {
            return Err(Flaw::Invalid("samples that are no count of 20 ms frames"));
        }
        let (at, size) = packet::lay_out(self.toc, pcm.len() / FRAME, LEAST_FRAME, packet)?;
        let mut input = [0.0f32; FRAME];
        for (frame, samples) in pcm.as_chunks::<FRAME>().0.iter().enumerate() {
            for (value, &sample) in input.iter_mut().zip(samples) {
                let sample = f32::from(sample);
                *value = sample - self.level;
                self.level = POLE_STEP * sample + (1.0 - POLE_STEP) * self.level;
            }
            self.final_range = self.celt.encode(&input, &mut packet[at + frame * size..][..size])?;
        }
        Ok(packet.len())
    }
}
