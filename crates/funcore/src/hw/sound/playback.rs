//! A file's sound read back as r1metal records it or as a phone does, at the card's rate and in one channel, from any sample or looped.

use super::fill;
use super::pace::Source;
use crate::base::resample::Resampler;
use crate::codec::Flaw;
use crate::codec::aac::{Config, Decoder, FRAME, RATE, read_track, track_samples};
use crate::codec::mp4::Soundtrack;
use alloc::borrow::Cow;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The error text of a file with sound of a kind r1metal does not play.
pub const UNSUPPORTED: &str = "sound of a kind r1metal does not play";

/// The log line of a packet that does not decode.
pub fn to_undecoded_line(number: usize, flaw: Flaw) -> String {
    format!("sound: packet {number} of what is played: {flaw:?}")
}

/// A file's sound: its index, and its packets read on demand.
pub trait Packets {
    fn index(&self) -> &Soundtrack;
    // False where the packet cannot be read, which whoever reads reports.
    fn read(&mut self, number: usize, bytes: &mut Vec<u8>) -> bool;
    fn note_flaw(&mut self, number: usize, flaw: Flaw);
}

/// A sound file held in memory, an MP4 file or a stream of ADTS, borrowed or owned.
pub struct MemoryFile<'a> {
    track: Soundtrack,
    file: Cow<'a, [u8]>,
}

impl<'a> MemoryFile<'a> {
    /// The sound of a file; why it cannot be read where it cannot.
    pub fn parse(file: impl Into<Cow<'a, [u8]>>) -> Result<MemoryFile<'a>, Flaw> {
        let file = file.into();
        let track = read_track(&file)?;
        Ok(MemoryFile { track, file })
    }
}

impl Packets for MemoryFile<'_> {
    fn index(&self) -> &Soundtrack {
        &self.track
    }

    fn read(&mut self, number: usize, bytes: &mut Vec<u8>) -> bool {
        let length = self.file.len() as u64;
        let Some(packet) = self.track.packets.get(number).filter(|packet| packet.fits(length)) else {
            return false;
        };
        bytes.clear();
        bytes.extend_from_slice(&self.file[packet.at as usize..][..packet.bytes as usize]);
        true
    }

    fn note_flaw(&mut self, _: usize, _: Flaw) {}
}

/// The samples of a file's sound, from its start or any other sample.
pub struct Playback<P> {
    packets: P,
    decoder: Decoder,
    // Brings the sound to the card's rate; none for sound that is at it.
    resampler: Option<Resampler>,
    next: usize,
    // Samples of the file's rate to pass over before the next that is played.
    lead: usize,
    // Samples of the file's rate that its edit lets be heard and are not decoded yet, and whether the resampler was told the sound has ended.
    left: u64,
    is_ended: bool,
    packet: Vec<u8>,
    samples: VecDeque<i16>,
}

impl<P: Packets> Playback<P> {
    /// The playback of low complexity AAC of one channel or two at a rate of the standard's; none of any other file, or of one whose index tells another rate or number of channels than its configuration does.
    pub fn new(packets: P) -> Option<Playback<P>> {
        let track = packets.index();
        let config = Config::read(&track.sound.configuration).ok()?;
        if track.sound.rate != config.rate() || usize::from(track.sound.channels) != config.channels() {
            return None;
        }
        let (lead, left) = (track.sound.lead as usize, track_samples(track));
        let resampler = (config.rate() != RATE).then(|| Resampler::new(config.rate(), RATE));
        Some(Playback {
            packets,
            decoder: Decoder::of(config),
            resampler,
            next: 0,
            lead,
            left,
            is_ended: false,
            packet: Vec::new(),
            samples: VecDeque::new(),
        })
    }

    // Decodes the next packet onto the samples that are ready, less those still to pass over and those past the end of the edit; one that cannot be read or decoded is silence. Once the file has no more, the resampler gives the samples its last ones come to.
    fn decode_next(&mut self) -> bool {
        if self.next >= self.packets.index().packets.len() || self.left == 0 {
            return self.finish();
        }
        let mut frame = [0i16; FRAME];
        if !self.packets.read(self.next, &mut self.packet) {
            // What the decoder holds of the packet before the gap would sound behind it.
            self.decoder.reset();
        } else if let Err(flaw) = self.decoder.decode(&self.packet, &mut frame) {
            self.packets.note_flaw(self.next, flaw);
        }
        self.next += 1;
        let skip = self.lead.min(FRAME);
        self.lead -= skip;
        let heard = (FRAME - skip).min(usize::try_from(self.left).unwrap_or(usize::MAX));
        self.left -= heard as u64;
        for &sample in &frame[skip..skip + heard] {
            match &mut self.resampler {
                Some(resampler) => resampler.push(sample, &mut self.samples),
                None => self.samples.push_back(sample),
            }
        }
        true
    }

    // Hands the resampler the end of the sound, once: true if it made samples of it.
    fn finish(&mut self) -> bool {
        let Some(resampler) = self.resampler.as_mut().filter(|_| !self.is_ended) else {
            return false;
        };
        self.is_ended = true;
        let before = self.samples.len();
        resampler.finish(&mut self.samples);
        self.samples.len() > before
    }

    /// Fills a block with the next samples and silence behind the last; false once none are left.
    pub fn fill_block(&mut self, block: &mut [i16]) -> bool {
        while self.samples.len() < block.len() && self.decode_next() {}
        if self.samples.is_empty() {
            return false;
        }
        fill(block, &mut self.samples);
        true
    }

    /// Fills a block, the first sample again behind the last; a file that gives none fills it with silence.
    pub fn fill_looped(&mut self, block: &mut [i16]) {
        for sample in block {
            let mut wraps = 0;
            while self.samples.is_empty() && wraps < 2 {
                if !self.decode_next() {
                    self.seek(0);
                    wraps += 1;
                }
            }
            *sample = self.samples.pop_front().unwrap_or(0);
        }
    }
}

impl<P: Packets> Source for Playback<P> {
    fn seek(&mut self, sample: u64) {
        // Sound brought to the card's rate is made from the samples about its place: the first of them is where the file is read from.
        let from = self.resampler.as_mut().map_or(sample, |resampler| resampler.seek(sample));
        // A packet's samples come out whole only behind the packet before it: decoding begins one earlier, and what that one gives is passed over.
        let at = from as usize + self.packets.index().sound.lead as usize;
        let first = (at / FRAME).saturating_sub(1);
        self.decoder.reset();
        (self.next, self.lead) = (first, at - first * FRAME);
        self.left = track_samples(self.packets.index()).saturating_sub(from);
        self.is_ended = false;
        self.samples.clear();
    }

    fn next(&mut self) -> i16 {
        while self.samples.is_empty() && self.decode_next() {}
        self.samples.pop_front().unwrap_or(0)
    }
}

#[cfg(test)]
pub(super) mod support {
    use super::*;
    use alloc::vec;

    pub(crate) fn round_once(file: &'static [u8]) -> Vec<i16> {
        let mut memory = MemoryFile::parse(file).expect("a file whose index reads");
        let (packets, lead) = (memory.index().packets.len(), memory.index().sound.lead as usize);
        let (mut decoder, mut packet, mut frame) = (Decoder::new(), Vec::new(), [0i16; FRAME]);
        for number in 0..packets {
            assert!(memory.read(number, &mut packet), "packet {number} lies in the file");
            assert!(decoder.decode(&packet, &mut frame).is_ok(), "packet {number} decodes");
        }
        let mut sound = Playback::new(memory).expect("one channel of our AAC");
        let mut samples = vec![0i16; packets * FRAME - lead + 1];
        sound.fill_looped(&mut samples);
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::super::record::Recorder;
    use super::*;
    use crate::codec::aac::{CONFIGURATION, DELAY};
    use crate::codec::mp4::Content;
    use crate::codec::mp4::Sample;
    use crate::codec::mp4::Sound;
    use alloc::vec;

    struct Stub {
        track: Soundtrack,
        packets: Vec<Vec<u8>>,
        unread: Option<usize>,
        flaws: Vec<usize>,
    }

    impl Packets for Stub {
        fn index(&self) -> &Soundtrack {
            &self.track
        }

        fn read(&mut self, number: usize, bytes: &mut Vec<u8>) -> bool {
            bytes.clear();
            bytes.extend_from_slice(&self.packets[number]);
            self.unread != Some(number)
        }

        fn note_flaw(&mut self, number: usize, _: Flaw) {
            self.flaws.push(number);
        }
    }

    fn record(samples: usize) -> Stub {
        let hum: Vec<i16> = (0..samples).map(|at| if at / 100 % 2 == 0 { 3000 } else { -3000 }).collect();
        let (mut recorder, mut packets) = (Recorder::new(96_000), Vec::new());
        recorder.take(&hum, |packet| packets.push(packet.to_vec()));
        recorder.finish(|packet| packets.push(packet.to_vec()));
        let index = packets.iter().map(|packet| Sample { at: 0, bytes: packet.len() as u32, time: 0, is_key: true });
        let sound = Sound {
            rate: RATE,
            frame: FRAME as u32,
            channels: 1,
            configuration: CONFIGURATION.to_vec(),
            lead: DELAY as u32,
        };
        let track = Soundtrack { sound, packets: index.collect(), heard: None };
        Stub { track, packets, unread: None, flaws: Vec::new() }
    }

    fn read_all<P: Packets>(playback: &mut Playback<P>) -> Vec<i16> {
        let (mut samples, mut block) = (Vec::new(), [0i16; 700]);
        while playback.fill_block(&mut block) {
            samples.extend_from_slice(&block);
        }
        samples
    }

    #[test]
    fn seek_any_sample() {
        let stub = record(40 * FRAME);
        let packets = stub.packets.len();
        let mut playback = Playback::new(stub).unwrap();
        let samples = read_all(&mut playback);
        assert_eq!(
            samples.len(),
            (packets * FRAME - DELAY).div_ceil(700) * 700,
            "the lead passed over, silence behind"
        );
        assert!(samples[5 * FRAME..6 * FRAME].iter().any(|&sample| sample != 0), "and the hum in between");
        for start in [0usize, 1, FRAME - 1, FRAME, 5 * FRAME + 300, 20_000] {
            playback.seek(start as u64);
            let actual: Vec<i16> = (0..2 * FRAME).map(|_| Source::next(&mut playback)).collect();
            assert_eq!(actual, samples[start..start + 2 * FRAME], "from sample {start}");
        }
        playback.seek((packets * FRAME) as u64);
        assert_eq!(Source::next(&mut playback), 0, "behind the last sample there is silence");
        assert!(!playback.fill_block(&mut [1; 8]), "and a block has none");
    }

    #[test]
    fn looped() {
        let stub = record(10 * FRAME);
        let length = stub.packets.len() * FRAME - DELAY;
        let mut playback = Playback::new(stub).unwrap();
        let mut samples = vec![0i16; 2 * length + 5];
        let (first, rest) = samples.split_at_mut(700);
        playback.fill_looped(first);
        playback.fill_looped(rest);
        assert!(samples[..length].iter().any(|&sample| sample != 0), "the hum");
        assert_eq!(
            samples[..length],
            samples[length..2 * length],
            "the last sample followed by the first, nothing between"
        );
        assert_eq!(samples[2 * length..], samples[..5]);
        let mut silent = Playback::new(record(0)).unwrap();
        let mut block = [5i16; 64];
        silent.fill_looped(&mut block);
        assert_eq!(block, [0; 64], "a file that gives nothing beyond its lead is silence");
    }

    #[test]
    fn memory_file() {
        let stub = record(10 * FRAME);
        let (mut writer, mut file) = Content::Sound(stub.track.sound.clone(), 0).begin();
        for packet in &stub.packets {
            writer.add_packet(packet.len() as u32);
            file.extend_from_slice(packet);
        }
        assert!(writer.finish().append_to(&mut file));
        let expected = read_all(&mut Playback::new(stub).unwrap());
        assert_eq!(read_all(&mut Playback::new(MemoryFile::parse(&file).unwrap()).unwrap()), expected);
        assert_eq!(
            read_all(&mut Playback::new(MemoryFile::parse(file.clone()).unwrap()).unwrap()),
            expected,
            "owned as well as borrowed"
        );
        assert!(MemoryFile::parse(&file[..40]).is_err(), "a file cut off before its index");
    }

    #[test]
    fn bad_packets_and_kinds() {
        let mut stub = record(10 * FRAME);
        let length = stub.packets.len();
        (stub.unread, stub.packets[5]) = (Some(3), vec![0xFF; 3]);
        let mut playback = Playback::new(stub).unwrap();
        let samples = read_all(&mut playback);
        assert!(samples[2 * FRAME..3 * FRAME].iter().all(|&sample| sample == 0), "the packet not read is silence");
        let mut alone = [0i16; FRAME];
        Decoder::new().decode(&record(10 * FRAME).packets[4], &mut alone).unwrap();
        assert!(
            samples[4 * FRAME - DELAY..5 * FRAME - DELAY] == alone,
            "the packet behind it decodes as a sound's first, with nothing of the one before the gap in it"
        );
        assert_eq!(playback.packets.flaws, [5], "the one that does not decode is told of");
        assert_eq!(to_undecoded_line(5, Flaw::Short), "sound: packet 5 of what is played: Short");
        assert_eq!(samples.len(), (length * FRAME - DELAY).div_ceil(700) * 700, "and the rest is played");
        let other = |change: fn(&mut Soundtrack)| {
            let mut stub = record(FRAME);
            change(&mut stub.track);
            Playback::new(stub).is_none()
        };
        assert!(other(|track| track.sound.channels = 2) && other(|track| track.sound.rate = 44_100));
        assert!(
            other(|track| track.sound.configuration = vec![0x12, 0x10]),
            "the index and the configuration say different things"
        );
        assert!(other(|track| track.sound.configuration = vec![0x29, 0x88]), "nor sound of another kind of AAC");
        assert!(other(|track| track.sound.configuration = vec![0x11]), "nor one whose configuration is cut short");
    }

    // Files of FFmpeg's AAC encoder with the tools phones do not have on too, of a signal that holds transients: 0.8 s of two channels at 44.1 kHz and of one at 48 kHz, in MP4 and as a stream, and of one at 16 kHz; and what FFmpeg's decoder makes of the ones at 48 kHz and 16 kHz, 16 bit samples at their own rate.
    const STEREO: &[u8] = include_bytes!("../../../assets/sound/stereo_44100.m4a");
    const MONO: &[u8] = include_bytes!("../../../assets/sound/mono_48000.m4a");
    const MONO_STREAM: &[u8] = include_bytes!("../../../assets/sound/mono_48000.aac");
    const MONO_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/mono_48000.pcm");
    const LOW: &[u8] = include_bytes!("../../../assets/sound/mono_16000.m4a");
    const LOW_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/mono_16000.pcm");
    // Two channels at 22.05 kHz with no noise in them, whose decoding is the same wherever it begins, and what FFmpeg's decoder makes of them.
    const PLAIN: &[u8] = include_bytes!("../../../assets/sound/stereo_22050.m4a");
    const PLAIN_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/stereo_22050.pcm");

    // The most a sample played lies from FFmpeg's decoding of the file at the card's rate, measured at 1 for the file at 48 kHz and at 1 for the one at 16 kHz brought to 48 kHz by `Resampler`; the bound is 2.
    const NEAREST: i32 = 2;
    // The same for two channels played as their mean against the mean of FFmpeg's channels brought to the card's rate, measured at 2: the rounding of FFmpeg's two channels and of the mean, added to what the decoders and the filter differ by; the bound is 3.
    const MEAN_NEAREST: i32 = 3;

    // Every sample a playback gives, one at a time so that none is made up to fill a block.
    fn exactly<P: Packets>(playback: &mut Playback<P>) -> Vec<i16> {
        let (mut samples, mut one) = (Vec::new(), [0i16; 1]);
        while playback.fill_block(&mut one) {
            samples.push(one[0]);
        }
        samples
    }

    // A file's sound at its own rate and in one channel, from its decoder: packet by packet, without its lead and no longer than its edit lets be heard.
    fn at_its_rate(file: &'static [u8]) -> (Vec<i16>, u32) {
        let mut memory = MemoryFile::parse(file).unwrap();
        let config = Config::read(&memory.index().sound.configuration).unwrap();
        let (mut decoder, mut packet, mut frame) = (Decoder::of(config), Vec::new(), [0i16; FRAME]);
        let mut samples = Vec::new();
        for number in 0..memory.index().packets.len() {
            assert!(memory.read(number, &mut packet));
            assert_eq!(decoder.decode(&packet, &mut frame), Ok(()));
            samples.extend_from_slice(&frame);
        }
        let (lead, length) = (memory.index().sound.lead as usize, track_samples(memory.index()) as usize);
        (samples[lead..lead + length].to_vec(), config.rate())
    }

    fn to_the_card(rate: u32, samples: &[i16]) -> Vec<i16> {
        let (mut resampler, mut out) = (Resampler::new(rate, RATE), Vec::new());
        samples.iter().for_each(|&sample| resampler.push(sample, &mut out));
        resampler.finish(&mut out);
        out
    }

    fn reference(file: &[u8]) -> Vec<i16> {
        file.as_chunks::<2>().0.iter().copied().map(i16::from_le_bytes).collect()
    }

    fn worst(played: &[i16], expected: &[i16]) -> Option<i32> {
        played.iter().zip(expected).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max()
    }

    #[test]
    fn stereo_as_mono_at_card_rate() {
        let mut playback = Playback::new(MemoryFile::parse(STEREO).unwrap()).unwrap();
        let played = exactly(&mut playback);
        let (native, rate) = at_its_rate(STEREO);
        assert_eq!((native.len(), rate), (35_280, 44_100), "the lead passed over and the end of the edit kept to");
        assert_eq!(played.len(), 38_400, "0.8 s at 48 kHz");
        assert!(played == to_the_card(rate, &native), "the mean of its channels, brought to the card's rate");
        assert!(played.iter().any(|&sample| sample.unsigned_abs() > 5000), "and there is sound in it");
        assert!(!playback.fill_block(&mut [1; 4]), "and no more");
    }

    #[test]
    fn mono_against_ffmpeg() {
        let (mut playback, mut low) = (
            Playback::new(MemoryFile::parse(MONO).unwrap()).unwrap(),
            Playback::new(MemoryFile::parse(LOW).unwrap()).unwrap(),
        );
        let (played, brought) = (exactly(&mut playback), exactly(&mut low));
        let expected = reference(MONO_FFMPEG);
        assert_eq!(played.len(), expected.len(), "as many samples as FFmpeg plays");
        assert!(worst(&played, &expected) <= Some(NEAREST), "{:?} from FFmpeg's samples", worst(&played, &expected));
        let at_16 = reference(LOW_FFMPEG);
        let expected = to_the_card(16_000, &at_16);
        assert_eq!(brought.len(), 3 * at_16.len(), "three samples for each");
        assert!(
            worst(&brought, &expected) <= Some(NEAREST),
            "{:?} from FFmpeg's samples brought up",
            worst(&brought, &expected)
        );
    }

    #[test]
    fn channel_mean_against_ffmpeg() {
        let both = reference(PLAIN_FFMPEG);
        let mean: Vec<i16> = both
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| ((f32::from(pair[0]) + f32::from(pair[1])) / 2.0 + 0.5).floor() as i16)
            .collect();
        let played = exactly(&mut Playback::new(MemoryFile::parse(PLAIN).unwrap()).unwrap());
        let expected = to_the_card(22_050, &mean);
        assert_eq!((played.len(), expected.len()), (38_400, 38_400), "0.8 s at 48 kHz");
        assert!(
            worst(&played, &expected) <= Some(MEAN_NEAREST),
            "{:?} from the mean of FFmpeg's channels",
            worst(&played, &expected)
        );
    }

    // QuickTime's layout of one channel at 16 kHz, and FFmpeg's decoding of it.
    const QUICKTIME: &[u8] = include_bytes!("../../../assets/sound/quicktime_16000.mov");
    const QUICKTIME_FFMPEG: &[u8] = include_bytes!("../../../assets/sound/quicktime_16000.pcm");

    #[test]
    fn quicktime_against_ffmpeg() {
        let played = exactly(&mut Playback::new(MemoryFile::parse(QUICKTIME).unwrap()).unwrap());
        let at_16 = reference(QUICKTIME_FFMPEG);
        let expected = to_the_card(16_000, &at_16);
        assert_eq!(played.len(), 3 * at_16.len(), "three samples for each");
        assert!(worst(&played, &expected) <= Some(NEAREST), "{:?} from FFmpeg's samples", worst(&played, &expected));
        assert!(played.iter().any(|&sample| sample.unsigned_abs() > 1000), "and there is sound in it");
    }

    #[test]
    fn owned_voice_notes() {
        for (name, bytes) in [("stereo_44100.m4a", STEREO), ("mono_48000.m4a", MONO), ("mono_16000.m4a", LOW)] {
            let played = exactly(&mut Playback::new(MemoryFile::parse(bytes.to_vec()).unwrap()).unwrap());
            assert_eq!(played.len(), 38_400, "{name}: 0.8 s at 48 kHz");
        }
        for (name, bytes) in [("mono_48000.aac", MONO_STREAM), ("quicktime_16000.mov", QUICKTIME)] {
            let played = exactly(&mut Playback::new(MemoryFile::parse(bytes.to_vec()).unwrap()).unwrap());
            assert!(played.iter().any(|&sample| sample != 0), "{name}: a stream of ADTS and QuickTime's file play too");
        }
        assert!(MemoryFile::parse(b"no voice note".to_vec()).is_err(), "bytes of no sound are refused");
    }

    #[test]
    fn stream_lead() {
        let memory = MemoryFile::parse(MONO_STREAM).unwrap();
        let packets = memory.index().packets.len();
        let (stream, file) = (
            exactly(&mut Playback::new(memory).unwrap()),
            exactly(&mut Playback::new(MemoryFile::parse(MONO).unwrap()).unwrap()),
        );
        assert_eq!(stream.len(), packets * FRAME, "every packet of a stream is played, its encoder's lead included");
        assert!(stream[FRAME..FRAME + file.len()] == file[..], "a file's lead is the stream's first frame");
    }

    #[test]
    fn resampled_seek_and_loop() {
        let mut playback = Playback::new(MemoryFile::parse(PLAIN).unwrap()).unwrap();
        let played = exactly(&mut playback);
        for from in [0usize, 1, 700, FRAME, 5 * FRAME + 300, 20_000, played.len() - 1000] {
            playback.seek(from as u64);
            let heard: Vec<i16> = (0..1000).map(|_| Source::next(&mut playback)).collect();
            assert!(heard == played[from..from + 1000], "from sample {from}");
        }
        playback.seek(played.len() as u64 - 3);
        let heard: Vec<i16> = (0..8).map(|_| Source::next(&mut playback)).collect();
        assert!(
            heard[..3] == played[played.len() - 3..] && heard[3..] == [0; 5],
            "behind the last sample there is silence"
        );
        playback.seek(2 * played.len() as u64);
        assert_eq!(Source::next(&mut playback), 0, "and far behind it");
        let mut round = vec![0i16; 2 * played.len() + 5];
        let mut again = Playback::new(MemoryFile::parse(PLAIN).unwrap()).unwrap();
        again.fill_looped(&mut round);
        assert!(
            round[..played.len()] == played[..] && round[played.len()..2 * played.len()] == played[..],
            "the last sample followed by the first"
        );
        assert!(round[2 * played.len()..] == played[..5]);
    }

    #[test]
    fn edit_end() {
        let mut stub = record(10 * FRAME);
        let whole = exactly(&mut Playback::new(record(10 * FRAME)).unwrap());
        stub.track.heard = Some(5000);
        let mut playback = Playback::new(stub).unwrap();
        let played = exactly(&mut playback);
        assert!(played[..] == whole[..5000], "the first 5000 samples");
        playback.seek(4000);
        let heard: Vec<i16> = (0..1100).map(|_| Source::next(&mut playback)).collect();
        assert!(
            heard[..1000] == whole[4000..5000] && heard[1000..] == [0; 100],
            "from the middle, and silence from where the edit ends"
        );
        let mut stub = record(10 * FRAME);
        stub.track.heard = Some(5000);
        let mut round = vec![0i16; 10_000];
        Playback::new(stub).unwrap().fill_looped(&mut round);
        assert!(
            round[..5000] == whole[..5000] && round[5000..] == whole[..5000],
            "round and round it is the part that is heard"
        );
    }

    #[test]
    fn standard_rates_and_channels() {
        let with = |configuration: Vec<u8>, rate: u32, channels: u16| {
            let mut stub = record(2 * FRAME);
            (stub.track.sound.configuration, stub.track.sound.rate, stub.track.sound.channels) =
                (configuration, rate, channels);
            Playback::new(stub).is_some()
        };
        assert!(
            with(vec![0x12, 0x10], 44_100, 2) && with(vec![0x11, 0x88], 48_000, 1) && with(vec![0x15, 0x88], 8_000, 1)
        );
        assert!(
            with(vec![0x11, 0x90], 48_000, 2) && with(vec![0x13, 0x08], 24_000, 1) && with(vec![0x10, 0x10], 96_000, 2)
        );
        assert!(!with(vec![0x12, 0x10], 44_100, 1), "two channels told, one in the index");
        assert!(!with(vec![0x11, 0x98], 48_000, 3), "three channels");
        assert!(!with(vec![0x12, 0x10, 0x56, 0xE5, 0x80], 44_100, 2), "spectral band replication");
        assert!(with(vec![0x12, 0x10, 0x56, 0xE5, 0x00], 44_100, 2), "which is told absent");
    }
}
