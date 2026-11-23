//! A recording: its mode, the encoder's settings, its track, its bit budget, its timeline and its statistics.

use crate::codec::h264::write as h264;
use crate::codec::mp4::VideoTrack;
use crate::hw::camera::exposure::FRAMES_PER_SECOND;
use crate::hw::camera::exposure::Pace;
use crate::hw::camera::frame::Frame;
use crate::hw::camera::mode::Mode;
use crate::hw::venc::Settings;
use crate::hw::venc::Shape;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// The sensor's mode for the video viewfinder and its recordings.
pub const MODE: Mode = Mode::SlimVideo;
const LEVEL: u32 = 41;
const GROUP: u32 = FRAMES_PER_SECOND;
const BITS_PER_SECOND: u64 = 16_000_000;
pub const SOUND_BITS_PER_SECOND: u32 = 96_000;
/// The error text of a partition without room for a recording.
pub const NO_ROOM: &str = "the data partition has no room for a recording";
/// The log line of a recording that ends for want of room.
pub const FULL_LINE: &str = "camera: the data partition is nearly full, the recording ends";

/// The error text of frames the encoder does not take.
pub fn to_unfit_text(frame: Frame) -> String {
    format!("the encoder takes no frames of {}x{}", frame.width(), frame.lines)
}

/// Packs the encoder's stream of frame `index` into `sample`; the error text where it holds no slice.
pub fn pack_frame(stream: &[u8], sample: &mut Vec<u8>, index: u32) -> Result<(), String> {
    if h264::pack(stream, sample) == 0 {
        return Err(format!("the encoder's {} bytes of frame {index} hold no slice", stream.len()));
    }
    Ok(())
}

/// The log line of a complete video file.
pub fn to_video_line(name: &str, samples: usize, packets: usize, bytes: u64, ticks: u64) -> String {
    format!("camera: video {name}: {samples} frames, {packets} packets of sound, {bytes} bytes, {ticks} ticks")
}

/// The encoder's settings for frames of that size; none where it takes no such frames or would have to pad them.
pub fn settings(frame: Frame) -> Option<Settings> {
    let shape = Shape { width: frame.width(), height: frame.lines };
    let settings = Settings { shape, level: LEVEL, group: GROUP, frames_per_second: FRAMES_PER_SECOND };
    let is_aligned = shape.stride() == frame.width() as usize && shape.lines() == frame.lines as usize;
    (settings.is_valid() && is_aligned).then_some(settings)
}

/// The video track of a recording at a pace, made at `creation_time`, turned by a player `quarters` quarter turns clockwise.
pub fn track(settings: &Settings, pace: Pace, quarters: u8, creation_time: u64) -> VideoTrack {
    let Settings { shape, level, .. } = *settings;
    let (ticks_per_frame, ticks_per_second) = pace.clock();
    let stream = h264::Stream {
        width: shape.width,
        height: shape.height,
        level,
        pace: Some((ticks_per_frame, ticks_per_second)),
        is_chroma_on_luma: true,
    };
    VideoTrack {
        width: shape.width,
        height: shape.height,
        ticks_per_second,
        ticks_per_frame,
        sequence_parameters: stream.sequence_parameters(),
        picture_parameters: stream.picture_parameters(),
        quarters,
        creation_time,
    }
}

/// Bits a frame of a recording at a pace aims for.
pub fn frame_bits(pace: Pace) -> u64 {
    let (ticks_per_frame, ticks_per_second) = pace.clock();
    BITS_PER_SECOND * u64::from(ticks_per_frame) / u64::from(ticks_per_second)
}

/// When a recording's frames show, by the sensor's count of the frames it sent.
pub struct Timeline {
    ticks_per_frame: u64,
    first: Option<u64>,
    last: u64,
}

impl Timeline {
    pub const fn new(ticks_per_frame: u32) -> Timeline {
        Timeline { ticks_per_frame: ticks_per_frame as u64, first: None, last: 0 }
    }

    /// Takes the `count`th frame the sensor sent: whether it is the first, and its time in ticks from the first.
    pub fn take_frame(&mut self, count: u64) -> (bool, u64) {
        let is_first = self.first.is_none();
        let first = *self.first.get_or_insert(count);
        self.last = count;
        (is_first, (count - first) * self.ticks_per_frame)
    }

    /// Frames the sensor sent from the first recorded to the last; none before the first.
    pub fn frames_sent(&self) -> u64 {
        self.first.map_or(0, |first| self.last - first + 1)
    }
}

/// How a recording went.
#[derive(Default)]
pub struct Stats {
    frames: u32,
    bytes: u64,
    convert_time: Duration,
    max_convert: Duration,
    encode_time: Duration,
    max_encode: Duration,
    wait_time: Duration,
    cycles: u64,
    quantisers: Option<(u32, u32)>,
}

impl Stats {
    /// The frames taken so far, which is the number of the next.
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// Takes a frame's picture, made in `time` and encoded at that quantiser.
    pub fn add_picture(&mut self, time: Duration, quantiser: u32) {
        self.convert_time += time;
        self.max_convert = self.max_convert.max(time);
        self.quantisers = Some(
            self.quantisers
                .map_or((quantiser, quantiser), |(finest, coarsest)| (finest.min(quantiser), coarsest.max(quantiser))),
        );
    }

    /// Takes a frame ended into `bytes`, `time` after the encoder's start, `wait` of it waited, in `cycles` of its clock.
    pub fn add_frame(&mut self, bytes: usize, time: Duration, wait: Duration, cycles: u32) {
        self.frames += 1;
        self.bytes += bytes as u64;
        self.encode_time += time;
        self.max_encode = self.max_encode.max(time);
        self.wait_time += wait;
        self.cycles += u64::from(cycles);
    }

    /// The log line of a recording of these frames out of `frames_sent` in `span`, turned by a player `quarters` quarter turns.
    pub fn to_log_line(&self, frames_sent: u64, span: Duration, quarters: u8) -> String {
        let frames = u64::from(self.frames.max(1));
        let mean = |total: Duration| total.as_micros() as u64 / frames;
        let (finest, coarsest) = self.quantisers.unwrap_or_default();
        format!(
            "camera: recorded {} frames of {frames_sent} in {:.1} s, {} bytes, {quarters} quarter turns for the player, quantisers {finest} to {coarsest}; picture made in {} us ({} at most), taken from the encoder {} us after its start ({} at most, {} of it waited, {} cycles)",
            self.frames,
            span.as_secs_f32(),
            self.bytes,
            mean(self.convert_time),
            self.max_convert.as_micros(),
            mean(self.encode_time),
            self.max_encode.as_micros(),
            mean(self.wait_time),
            self.cycles / frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_settings() {
        let actual = settings(MODE.frame()).expect("the encoder takes slim video's frames");
        assert_eq!(
            (actual.shape.stride(), actual.shape.lines()),
            (1280, 720),
            "whole macroblocks, so nothing has to be padded"
        );
        assert_eq!((actual.level, actual.group, actual.frames_per_second), (41, 60, 60), "a key frame a second");
        assert_eq!(
            (settings(Frame::whole(1282, 720)), settings(Frame::whole(0, 720))),
            (None, None),
            "frames the encoder would pad, and frames of nothing"
        );
        assert_eq!(frame_bits(Pace::LIVE), 266_633, "bits a frame");
    }

    #[test]
    fn track_fields() {
        let actual = settings(MODE.frame()).unwrap();
        let track = track(&actual, Pace::LIVE, 3, 1_791_500_000);
        let stream =
            h264::Stream { width: 1280, height: 720, level: 41, pace: Some((7999, 480_000)), is_chroma_on_luma: true };
        assert_eq!(
            (
                track.width,
                track.height,
                track.ticks_per_frame,
                track.ticks_per_second,
                track.quarters,
                track.creation_time
            ),
            (1280, 720, 7999, 480_000, 3, 1_791_500_000)
        );
        assert_eq!(
            (track.sequence_parameters, track.picture_parameters),
            (stream.sequence_parameters(), stream.picture_parameters())
        );
    }

    #[test]
    fn slice_required() {
        let mut sample = Vec::new();
        assert_eq!(pack_frame(&[0, 0, 1, 0x65], &mut sample, 3), Ok(()));
        assert_eq!(pack_frame(&[1, 2], &mut sample, 3), Err("the encoder's 2 bytes of frame 3 hold no slice".into()));
        assert_eq!(to_unfit_text(Frame::whole(1282, 720)), "the encoder takes no frames of 1282x720");
        assert_eq!(
            to_video_line("a.mp4", 2, 3, 4, 5),
            "camera: video a.mp4: 2 frames, 3 packets of sound, 4 bytes, 5 ticks"
        );
    }

    #[test]
    fn timeline_and_stats() {
        let mut timeline = Timeline::new(7999);
        assert_eq!(timeline.frames_sent(), 0);
        assert_eq!(
            [timeline.take_frame(41), timeline.take_frame(42), timeline.take_frame(45)],
            [(true, 0), (false, 7999), (false, 4 * 7999)]
        );
        assert_eq!(timeline.frames_sent(), 5, "a frame lost on the way counts as sent");
        let mut stats = Stats::default();
        stats.add_picture(Duration::from_micros(300), 30);
        stats.add_picture(Duration::from_micros(500), 24);
        stats.add_frame(40_000, Duration::from_micros(9000), Duration::from_micros(1000), 2_800_000);
        stats.add_frame(20_000, Duration::from_micros(7000), Duration::from_micros(3000), 2_600_000);
        assert_eq!(
            stats.to_log_line(timeline.frames_sent(), Duration::from_millis(2500), 1),
            "camera: recorded 2 frames of 5 in 2.5 s, 60000 bytes, 1 quarter turns for the player, quantisers 24 to 30; picture made in 400 us (500 at most), taken from the encoder 8000 us after its start (9000 at most, 2000 of it waited, 2700000 cycles)"
        );
        assert!(
            Stats::default().to_log_line(0, Duration::ZERO, 0).contains("recorded 0 frames of 0"),
            "nothing recorded"
        );
    }
}
