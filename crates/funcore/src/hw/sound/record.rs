//! Microphone samples on their way into a file: settled, levelled, encoded and lined up with the file's start.

use super::level::Leveller;
use super::samples_in;
use crate::codec::aac::{CONFIGURATION, DELAY, Encoder, FRAME, RATE};
use crate::codec::mp4::Sound;
use alloc::vec::Vec;
use core::time::Duration;

/// Samples a fresh microphone settles for at the card's rate, recorded as silence: 60 ms, a fifth above the time measured.
pub const SETTLE_SAMPLES: usize = 2880;
const FADE_SAMPLES: usize = 960;

// A fresh microphone's settling, at the rate it is read at: how many of its first samples are still to be silence.
pub(crate) struct Settle {
    left: usize,
}

impl Settle {
    pub(crate) const fn new(rate: u32) -> Settle {
        Settle { left: SETTLE_SAMPLES * rate as usize / RATE as usize }
    }

    /// How many of the microphone's next `count` samples are silence, which counts them as passed.
    pub fn take(&mut self, count: usize) -> usize {
        let silent = self.left.min(count);
        self.left -= silent;
        silent
    }
}

/// Bits a second of a recorded voice: clear enough for a listener and the model that transcribes it.
pub const VOICE_BITS_PER_SECOND: u32 = 64_000;

/// The sound of a recording in its file: one channel of AAC at the card's rate.
pub fn sound_format() -> Sound {
    Sound { rate: RATE, frame: FRAME as u32, channels: 1, configuration: CONFIGURATION.to_vec(), lead: DELAY as u32 }
}

pub(super) struct Recorder {
    leveller: Leveller,
    encoder: Encoder,
    frame: Vec<i16>,
    block: Vec<i16>,
    settle: Settle,
    // Samples taken since the microphone settled.
    settled: usize,
    packet: Vec<u8>,
}

impl Recorder {
    pub(super) fn new(bits_per_second: u32) -> Recorder {
        Recorder {
            leveller: Leveller::default(),
            encoder: Encoder::new(bits_per_second),
            frame: Vec::with_capacity(FRAME),
            block: Vec::new(),
            settle: Settle::new(RATE),
            settled: 0,
            packet: Vec::new(),
        }
    }

    fn fill(&mut self, mut samples: &[i16], out: &mut impl FnMut(&[u8])) {
        while !samples.is_empty() {
            let (head, tail) = samples.split_at(samples.len().min(FRAME - self.frame.len()));
            self.frame.extend_from_slice(head);
            samples = tail;
            if self.frame.len() == FRAME {
                self.encoder.encode(&self.frame, &mut self.packet);
                out(&self.packet);
                self.frame.clear();
            }
        }
    }

    pub(super) fn take(&mut self, samples: &[i16], mut out: impl FnMut(&[u8])) {
        let mut block = core::mem::take(&mut self.block);
        block.clear();
        let silent = self.settle.take(samples.len());
        block.resize(silent, 0);
        block.extend(samples[silent..].iter().enumerate().map(|(index, &sample)| {
            let fade = (self.settled + index).min(FADE_SAMPLES);
            if fade == FADE_SAMPLES { sample } else { (i32::from(sample) * fade as i32 / FADE_SAMPLES as i32) as i16 }
        }));
        self.settled += samples.len() - silent;
        self.leveller.level(&mut block);
        self.fill(&block, &mut out);
        self.block = block;
    }

    const fn is_faded_in(&self) -> bool {
        self.settled > FADE_SAMPLES
    }

    fn silence(&mut self, mut samples: usize, mut out: impl FnMut(&[u8])) {
        const ZEROS: [i16; FRAME] = [0; FRAME];
        while samples > 0 {
            let count = samples.min(FRAME);
            self.fill(&ZEROS[..count], &mut out);
            samples -= count;
        }
    }

    pub(super) fn finish(&mut self, mut out: impl FnMut(&[u8])) {
        let padding = (FRAME - self.frame.len()) % FRAME;
        self.silence(padding + FRAME, &mut out);
    }
}

/// Where a recording's packets go.
pub trait Outlet {
    // An empty buffer for a packet's bytes.
    fn buffer(&mut self) -> Vec<u8>;
    fn send(&mut self, packet: Vec<u8>);
}

/// A recording lined up with its file's start; packets wait until the sound has faded in, so a shorter recording gives its file none.
pub struct Lineup {
    recorder: Recorder,
    early_samples: u64,
    excess: Option<u64>,
    pending: Vec<Vec<u8>>,
}

impl Lineup {
    /// A recording of `bits_per_second` on average, its file's start not known yet.
    pub fn new(bits_per_second: u32) -> Lineup {
        Lineup { recorder: Recorder::new(bits_per_second), early_samples: 0, excess: None, pending: Vec::new() }
    }

    fn run(&mut self, outlet: &mut impl Outlet, step: impl FnOnce(&mut Recorder, &mut dyn FnMut(&[u8]))) {
        let pending = &mut self.pending;
        step(&mut self.recorder, &mut |packet| {
            let mut bytes = outlet.buffer();
            bytes.extend_from_slice(packet);
            pending.push(bytes);
        });
        if self.recorder.is_faded_in() {
            self.pending.drain(..).for_each(|bytes| outlet.send(bytes));
        }
    }

    /// Takes the microphone's next block; `began` is how much later and how much earlier than the file it began, none while either start is unknown, and such a block goes unrecorded.
    pub fn take(&mut self, samples: &[i16], began: Option<(Duration, Duration)>, outlet: &mut impl Outlet) {
        if self.excess.is_none() {
            let Some((late, early)) = began else {
                self.early_samples += samples.len() as u64;
                return;
            };
            let gap = samples_in(late, RATE) as usize;
            self.run(outlet, |recorder, out| recorder.silence(gap, out));
            self.excess = Some(samples_in(early, RATE).saturating_sub(self.early_samples));
        }
        let excess = self.excess.get_or_insert(0);
        let skip = (*excess).min(samples.len() as u64) as usize;
        *excess -= skip as u64;
        self.run(outlet, |recorder, out| recorder.take(&samples[skip..], out));
    }

    /// Puts silence for lost samples once the file's start is known; before that they go unrecorded.
    pub fn add_lost(&mut self, samples: usize, outlet: &mut impl Outlet) {
        if self.excess.is_some() {
            self.run(outlet, |recorder, out| recorder.silence(samples, out));
        } else {
            self.early_samples += samples as u64;
        }
    }

    /// Ends the recording, flushing the recorder for a file whose start is known.
    pub fn end(&mut self, outlet: &mut impl Outlet) {
        if self.excess.is_some() {
            self.run(outlet, |recorder, out| recorder.finish(out));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::aac::Decoder;
    use alloc::vec;

    const FADE_END: usize = SETTLE_SAMPLES + FADE_SAMPLES;

    #[test]
    fn settle_by_rate() {
        let mut settle = Settle::new(RATE);
        assert_eq!(
            [settle.take(SETTLE_SAMPLES - 10), settle.take(30), settle.take(30)],
            [SETTLE_SAMPLES - 10, 10, 0],
            "the first 60 ms of a fresh microphone are silence, however its blocks fall"
        );
        assert_eq!(Settle::new(RATE / 2).take(usize::MAX), SETTLE_SAMPLES / 2, "the same time at half the rate");
    }

    fn hum(samples: usize, size: i16) -> Vec<i16> {
        (0..samples).map(|at| if at / 100 % 2 == 0 { size } else { -size }).collect()
    }

    fn play_back(blocks: usize, block: usize) -> (Vec<i16>, usize) {
        let (mut recorder, mut decoder) = (Recorder::new(96_000), Decoder::new());
        let (mut packets, mut samples) = (0, Vec::new());
        let mut play = |packet: &[u8]| {
            let mut frame = [0i16; FRAME];
            decoder.decode(packet, &mut frame).unwrap();
            samples.extend_from_slice(&frame);
            packets += 1;
        };
        for _ in 0..blocks {
            recorder.take(&hum(block, 2000), &mut play);
        }
        recorder.finish(&mut play);
        (samples, packets)
    }

    #[test]
    fn frames_and_flush() {
        let (samples, packets) = play_back(30, 700);
        assert_eq!(packets, (30 * 700usize).div_ceil(FRAME) + 1, "the frames of 21000 samples and one to flush");
        assert_eq!(samples.len(), packets * FRAME);
        let (even, same) = (play_back(21, 1000).1, play_back(1, 21_000).1);
        assert_eq!((even, same), (22, 22));
        assert_eq!(play_back(5, FRAME).1, 6, "a recording that ends with a frame gets the one to flush and no other");
    }

    #[test]
    fn settle_and_fade() {
        let (samples, _) = play_back(20, FRAME);
        let heard = &samples[FRAME..];
        let loudest = |samples: &[i16]| samples.iter().map(|sample| sample.unsigned_abs()).max().unwrap();
        assert_eq!(loudest(&heard[..FRAME]), 0, "nothing in the frame that ends before the sound comes in");
        assert!(
            loudest(&heard[FRAME..SETTLE_SAMPLES - 200]) < 400,
            "{} while it settles",
            loudest(&heard[FRAME..SETTLE_SAMPLES - 200])
        );
        assert!(loudest(&heard[FADE_END + 200..8 * FRAME]) > 1500, "and the hum after");
        let mut recorder = Recorder::new(96_000);
        let mut sizes = vec![];
        recorder.silence(3 * FRAME, |packet| sizes.push(packet.len()));
        assert_eq!(sizes, [4; 3], "silence put in takes four bytes a frame");
        assert!(!recorder.is_faded_in(), "and is nothing heard");
        recorder.take(&hum(FADE_END, 2000), |_| {});
        assert!(!recorder.is_faded_in(), "nor are the first moments and the coming in");
        recorder.take(&hum(1, 2000), |_| {});
        assert!(recorder.is_faded_in(), "the sample behind them is the sound whole");
    }

    #[derive(Default)]
    struct Stub(Vec<Vec<u8>>);

    impl Outlet for Stub {
        fn buffer(&mut self) -> Vec<u8> {
            Vec::new()
        }

        fn send(&mut self, packet: Vec<u8>) {
            self.0.push(packet);
        }
    }

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    #[test]
    fn late_microphone() {
        let (mut lineup, mut stub) = (Lineup::new(96_000), Stub::default());
        for _ in 0..30 {
            lineup.take(&hum(FRAME, 3000), Some((ms(100), Duration::ZERO)), &mut stub);
        }
        lineup.end(&mut stub);
        assert_eq!(stub.0.len(), 36, "30 blocks behind 4800 samples of silence, and one to flush");
    }

    #[test]
    fn early_and_lost() {
        let (mut lineup, mut stub) = (Lineup::new(96_000), Stub::default());
        for _ in 0..10 {
            lineup.take(&hum(FRAME, 3000), None, &mut stub);
        }
        lineup.add_lost(FRAME, &mut stub);
        assert_eq!(
            (lineup.early_samples, lineup.excess, stub.0.len()),
            (11 * FRAME as u64, None, 0),
            "nothing is kept before the beginning is known"
        );
        lineup.take(&hum(FRAME, 3000), Some((Duration::ZERO, ms(250))), &mut stub);
        assert_eq!(lineup.excess, Some(0), "of 12000 samples before the file 736 were still to leave out");
        for _ in 1..20 {
            lineup.take(&hum(FRAME, 3000), None, &mut stub);
        }
        lineup.add_lost(2 * FRAME, &mut stub);
        lineup.end(&mut stub);
        assert_eq!(stub.0.len(), 22 + 1, "20 blocks less 736 samples and 2048 of silence, and one to flush");
    }

    #[test]
    fn empty_recordings() {
        let (mut never, mut stub) = (Lineup::new(96_000), Stub::default());
        never.take(&hum(FRAME, 3000), None, &mut stub);
        never.end(&mut stub);
        assert!(stub.0.is_empty());
        let packets = |samples: usize| {
            let (mut lineup, mut stub) = (Lineup::new(96_000), Stub::default());
            lineup.take(&hum(samples, 3000), Some((ms(100), Duration::ZERO)), &mut stub);
            lineup.end(&mut stub);
            stub.0.len()
        };
        assert_eq!(packets(FADE_END), 0, "the silence before it, its first moments and its coming in");
        assert_eq!(
            packets(FADE_END + 1),
            (4800 + FADE_END + 1).div_ceil(FRAME) + 1,
            "one sample more is the sound whole, and the file has all that waited"
        );
    }
}
