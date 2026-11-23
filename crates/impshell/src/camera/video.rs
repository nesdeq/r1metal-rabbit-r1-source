use super::frames::Frames;
use super::rig::Stream;
use super::swivel::Swivel;
use super::{Rig, Shared, Stop};
use crate::crew::Crew;
use crate::media::reel::{PendingFile, Reel, Summary};
use crate::media::venc::Venc;
use crate::sound::{self, Recording};
use crate::storage::Data;
use crate::{clock, log, sys};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::base::planes::{self, part_lines};
use r1_funcore::base::time::stamp::to_stamp;
use r1_funcore::codec::mp4::Content;
use r1_funcore::domain::camera::{self, Outcome, Stats, Viewfinder};
use r1_funcore::hw::PARTS;
use r1_funcore::hw::camera::camsv::FrameCount;
use r1_funcore::hw::camera::exposure::{Auto, Pace};
use r1_funcore::hw::camera::frame::Frame;
use r1_funcore::hw::camera::recording::{self, FULL_LINE, MODE, NO_ROOM, SOUND_BITS_PER_SECOND, Timeline};
use r1_funcore::hw::camera::tone::Tone;
use r1_funcore::hw::camera::video::{Chroma, NO_PLANES, Planes, Scratch};
use r1_funcore::hw::rate::Rate;
use r1_funcore::hw::sound::record::sound_format;
use r1_funcore::storage::{VIDEO_EXTENSION, VIDEOS, is_full};
use std::io;
use std::os::fd::RawFd;
use std::time::Instant;

struct PendingFrame {
    is_key: bool,
    time: u64,
    start: Instant,
}

struct VideoRecording {
    encoder: Venc,
    planes: Planes,
    scratch: [Scratch; PARTS],
    rate: Rate,
    reel: Reel,
    sound: Option<Recording>,
    quarters: u8,
    timeline: Timeline,
    pending: Option<PendingFrame>,
    start: Instant,
    stats: recording::Stats,
}

impl VideoRecording {
    fn open(
        floor: RawFd,
        data: &Data,
        frame: Frame,
        quarters: u8,
        tone: &Tone,
        pace: Pace,
    ) -> Result<VideoRecording, Stop> {
        let Some(settings) = recording::settings(frame) else {
            return Err(io::Error::other(recording::to_unfit_text(frame)).into());
        };
        let shape = settings.shape;
        if is_full(data.free().ok()) {
            return Err(Stop { error: io::Error::other(NO_ROOM), outcome: Some(Outcome::Full) });
        }
        let planes =
            Planes::new(frame, shape.stride(), tone).ok_or_else(|| io::Error::other(NO_PLANES))?.with_lens(MODE.lens());
        let encoder = Venc::open(floor, settings)?;
        let track = recording::track(&settings, pace, quarters, sys::realtime_seconds());
        let timeline = Timeline::new(track.ticks_per_frame);
        let frame_bits = recording::frame_bits(pace);
        let (place, stem) = (data.clone(), to_stamp(clock::local()));
        let content = Content::Video(track, sound_format());
        let (reel, sender) = Reel::start(move || place.create(VIDEOS, &stem, VIDEO_EXTENSION), content)?;
        // A recording whose microphone cannot be had is made without sound.
        let sound = sound::record(sender, SOUND_BITS_PER_SECOND)
            .inspect_err(|e| log!("camera: a recording without sound: {e}"))
            .ok();
        Ok(VideoRecording {
            encoder,
            planes,
            scratch: Default::default(),
            rate: Rate::new(frame_bits, settings.group),
            reel,
            sound,
            quarters,
            timeline,
            pending: None,
            start: Instant::now(),
            stats: recording::Stats::default(),
        })
    }

    fn next_frame(&mut self, crew: &Crew, raw: &[u8], count: u64) -> io::Result<()> {
        self.end_frame()?;
        self.start_frame(crew, raw, count)
    }

    fn start_frame(&mut self, crew: &Crew, raw: &[u8], count: u64) -> io::Result<()> {
        let start = Instant::now();
        let settings = self.encoder.settings();
        let (stride, lines) = (settings.shape.stride(), settings.shape.lines());
        let index = self.stats.frames();
        let is_key = settings.is_key_frame(index);
        let quantiser = self.rate.quantiser(is_key);
        let (luma, chroma) = self.encoder.source().split_at_mut(stride * lines);
        {
            let planes = &self.planes;
            let mut pieces = planes::parts(luma, chroma, stride, part_lines(lines, PARTS)).zip(&mut self.scratch);
            let mut parts: [_; PARTS] = std::array::from_fn(|_| pieces.next());
            crew.for_each(&mut parts, |_, part| {
                if let Some(((lines, luma, chroma), scratch)) = part {
                    planes.convert(raw, lines.clone(), luma, Chroma::Pairs(chroma), scratch);
                }
            });
        }
        self.encoder.start(index, quantiser)?;
        self.stats.add_picture(start.elapsed(), quantiser);
        let (is_first, time) = self.timeline.take_frame(count);
        if is_first && let Some(sound) = &self.sound {
            sound.begin(start);
        }
        self.pending = Some(PendingFrame { is_key, time, start: Instant::now() });
        Ok(())
    }

    fn end_frame(&mut self) -> io::Result<()> {
        let Some(PendingFrame { is_key, time, start }) = self.pending.take() else {
            return Ok(());
        };
        let wait_start = Instant::now();
        let ending = self.encoder.wait()?;
        let (wait_time, elapsed, index) = (wait_start.elapsed(), start.elapsed(), self.stats.frames());
        let (stream, cycles) =
            self.encoder.stream(ending).map_err(|e| io::Error::other(format!("frame {index}: {e}")))?;
        let mut sample = self.reel.empty_buffer();
        sample.reserve(stream.len());
        recording::pack_frame(stream, &mut sample, index).map_err(io::Error::other)?;
        self.rate.take_frame(is_key, sample.len());
        self.stats.add_frame(sample.len(), elapsed, wait_time, cycles);
        self.reel.send_frame(sample, is_key, time)
    }

    fn close(mut self) -> PendingFile {
        if let Err(e) = self.end_frame() {
            log!("camera: the last frame of the recording: {e}");
        }
        log!("{}", self.stats.to_log_line(self.timeline.frames_sent(), self.start.elapsed(), self.quarters));
        // The microphone ends first; its last packets reach the file behind the last frame.
        drop(self.sound.take());
        self.reel.close()
    }
}

#[derive(Default)]
struct Recordings {
    current: Option<VideoRecording>,
    pending_files: Vec<(PendingFile, Option<Outcome>)>,
}

impl Recordings {
    fn end_current(&mut self, shared: &Shared, outcome: Option<Outcome>) {
        if let Some(recording) = self.current.take() {
            shared.set_recording(false);
            self.pending_files.push((recording.close(), outcome));
        }
    }

    fn finish_at(&mut self, shared: &Shared, index: usize) {
        let (file, outcome) = self.pending_files.swap_remove(index);
        shared.set_outcome(file_outcome(file.finish(), outcome));
    }

    fn finish_done(&mut self, shared: &Shared) {
        let mut index = 0;
        while index < self.pending_files.len() {
            if self.pending_files[index].0.is_done() {
                self.finish_at(shared, index);
            } else {
                index += 1;
            }
        }
    }

    fn finish_all(&mut self, shared: &Shared) {
        while !self.pending_files.is_empty() {
            self.finish_at(shared, 0);
        }
    }
}

fn file_outcome(result: io::Result<Option<Summary>>, outcome: Option<Outcome>) -> Outcome {
    match &result {
        Ok(Some(Summary { name, samples, packets, bytes, ticks })) => {
            log!("{}", recording::to_video_line(name, *samples, *packets, *bytes, *ticks));
        }
        Ok(None) => {}
        Err(e) => log!("camera: video file: {e}"),
    }
    let file = result.as_ref().ok().map(|summary| summary.as_ref().map(|summary| summary.name.as_str()));
    Outcome::from_recording(file, outcome)
}

impl Rig<'_> {
    pub(super) fn run_videos(
        &mut self,
        shared: &Shared,
        swivel: &mut Swivel,
        (viewfinder, format): (Viewfinder, PixelFormat),
        auto: &mut Auto,
        slots: &mut Option<Frames>,
    ) -> Result<Stats, Stop> {
        let (slots, finder) = self.bring_up(swivel, (viewfinder, format), auto, slots)?;
        self.start_sensor(slots.frame())?;
        let (mut stats, mut recordings) = (Stats::default(), Recordings::default());
        let outcome = self.start_stream(slots, &finder, auto).map_err(Stop::from).and_then(|mut stream| {
            self.show_and_record(shared, viewfinder, &mut stream, auto, &mut stats, &mut recordings)
        });
        recordings.end_current(shared, None);
        self.power_down();
        recordings.finish_all(shared);
        outcome.map(|()| stats)
    }

    fn show_and_record(
        &mut self,
        shared: &Shared,
        viewfinder: Viewfinder,
        stream: &mut Stream,
        auto: &mut Auto,
        stats: &mut Stats,
        recordings: &mut Recordings,
    ) -> Result<(), Stop> {
        // Frames the sensor has sent since the start, those included that no buffer was free for.
        let mut sensor_frames = FrameCount::default();
        loop {
            recordings.finish_done(shared);
            let Some(full) = self.next_full(stream, stats)? else {
                continue;
            };
            let count = sensor_frames.add(full.count, full.drops);
            let drawn = self.draw_frame(shared, stream, full.slot, stats)?;
            stream.show(shared, viewfinder);
            // The screen has its picture first, as soon after a flip with a recording as without; a click, which begins a recording or ends one, holds no picture up either.
            let is_running = recordings.current.is_some();
            let step = shared.board.lock().next_recording(viewfinder, stream.frame_count(), is_running);
            match step {
                camera::Recording::Leave => return Ok(()),
                camera::Recording::Show => {}
                camera::Recording::End => recordings.end_current(shared, None),
                camera::Recording::Begin(quarters) => {
                    let (frame, pace) = (stream.frame(), auto.pace());
                    match VideoRecording::open(self.floor(), &shared.data, frame, quarters, stream.tone(), pace) {
                        Ok(recording) => {
                            recordings.current = Some(recording);
                            shared.set_recording(true);
                        }
                        Err(stop) => {
                            log!("camera: no recording: {}", stop.error);
                            shared.set_outcome(stop.outcome.unwrap_or(Outcome::NoVideo));
                        }
                    }
                }
            }
            // Then the frame before goes to the file and this one to the encoder.
            if let Some(recording) = &mut recordings.current
                && let Err(e) = recording.next_frame(self.crew(), drawn.raw, count)
            {
                log!("camera: recording: {e}");
                recordings.end_current(shared, Some(Outcome::NoVideo));
            }
            if self.requeue(stream, drawn, auto, stats)?
                && let Some(recording) = &mut recordings.current
            {
                recording.planes.set_tone(stream.tone());
            }
            if stats.is_room_look_due(recordings.current.is_some()) && is_full(shared.data.free().ok()) {
                log!("{FULL_LINE}");
                recordings.end_current(shared, Some(Outcome::Full));
            }
        }
    }
}
