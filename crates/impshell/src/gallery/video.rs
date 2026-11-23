use super::{Context, Decoders, Shared};
use crate::crew::Crew;
use crate::log;
use crate::media::recording::{SoundFile, VideoFile};
use crate::media::vdec::Decoder;
use crate::sound::{self, SoundStream};
use crate::storage::Data;
use crate::sys;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::h264::read::Sequence;
use r1_funcore::codec::mp4;
use r1_funcore::domain::gallery::feed::{
    self, DrawStep, Feed, FirstStep, NO_BUFFER, NO_PICTURE, NO_SAMPLE, PICTURES_OUT, SampleStart,
};
use r1_funcore::domain::gallery::items::Item;
use r1_funcore::domain::gallery::player::{PlayStep, Player, VideoStep};
use r1_funcore::domain::gallery::projection::{self, upright_size};
use r1_funcore::domain::gallery::show::{SoundClock, due_ns, time_scale};
use r1_funcore::domain::gallery::stats::{self, PlayStats};
use r1_funcore::domain::gallery::{Kind, Label, RollRequest, Room, Thumbnail};
use r1_funcore::gfx::{Picture, bands};
use r1_funcore::hw::FRAME_PARTS;
use r1_funcore::hw::projector::{Projector, Scratch};
use r1_funcore::input::pose::Pose;
use r1_funcore::storage::VIDEOS;
use std::io;
use std::sync::Arc;
use std::time::Instant;

fn unsupported_size(sequence: &Sequence) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, projection::to_unsupported_text(sequence))
}

fn projector(sequence: &Sequence, quarters: u8, format: PixelFormat, room: Room, pose: Pose) -> io::Result<Projector> {
    projection::screen_projector(sequence, quarters, format, room, pose).ok_or_else(|| unsupported_size(sequence))
}

fn project(
    crew: &Crew,
    projector: &Projector,
    frame: &[u8],
    scratch: &mut [Scratch; FRAME_PARTS],
    picture: &mut Picture,
) {
    let (width, height) = projector.size();
    picture.words.resize(width * height, 0);
    picture.width = width;
    let mut pieces = bands(&mut picture.words, width, FRAME_PARTS).zip(scratch);
    let mut parts: [_; FRAME_PARTS] = std::array::from_fn(|_| pieces.next());
    crew.for_each(&mut parts, |_, band| {
        if let Some(((lines, words), scratch)) = band {
            projector.draw(frame, lines.clone(), words, scratch);
        }
    });
}

struct Playback<'a> {
    decoder: Decoder,
    decoders: &'a Decoders,
    scratch: [Scratch; FRAME_PARTS],
    video: VideoFile,
    sequence: Sequence,
    format: PixelFormat,
    projector: Projector,
    scale: (u64, u64),
    feed: Feed,
    sample: Vec<u8>,
    stats: PlayStats,
}

impl<'a> Playback<'a> {
    fn open(
        context: Context<'a>,
        name: &str,
        to_projector: impl FnOnce(&Sequence, u8) -> Option<Projector>,
    ) -> io::Result<Playback<'a>> {
        let video = VideoFile::open(context.data.open(VIDEOS, name)?)?;
        let sequence = video.sequence()?;
        let movie = &video.movie;
        let first_key = movie.first_key().ok_or_else(|| sys::invalid(NO_SAMPLE))?;
        let projector = to_projector(&sequence, movie.quarters).ok_or_else(|| unsupported_size(&sequence))?;
        let scale = time_scale(movie.frame_ticks(), movie.ticks_per_second, context.refresh_ns);
        let mut decoder = context.decoders.open_video(&sequence, PICTURES_OUT)?;
        let result = movie
            .sequence_parameters
            .iter()
            .chain(&movie.picture_parameters)
            .try_for_each(|unit| decoder.take_parameter_set(unit));
        if let Err(e) = result {
            context.decoders.keep_video(decoder);
            return Err(e);
        }
        Ok(Playback {
            decoder,
            decoders: context.decoders,
            scratch: Default::default(),
            video,
            sequence,
            format: context.format,
            projector,
            scale,
            feed: Feed::new(first_key),
            sample: Vec::new(),
            stats: PlayStats::default(),
        })
    }

    fn to_shown_line(&self, name: &str, pose: Pose) -> String {
        stats::to_shown_line(name, self.sequence.size(), &self.video.movie, self.projector.size(), pose)
    }

    fn turn(&mut self, room: Room, pose: Pose) -> io::Result<()> {
        self.projector = projector(&self.sequence, self.video.movie.quarters, self.format, room, pose)?;
        Ok(())
    }

    fn play_sound(&self, context: Context, name: &str, shared: &Shared) -> Option<SoundStream> {
        // A file without a track of sound is no fault: it plays silent, with nothing to tell.
        let file = context.data.open(VIDEOS, name).and_then(SoundFile::open);
        let file = file.inspect_err(|e| log!("gallery: {name}: no sound: {e}")).ok().flatten()?;
        let board = Arc::clone(&shared.board);
        let clock = SoundClock {
            scale: self.scale,
            ticks_per_second: self.video.movie.ticks_per_second,
            rate: file.track.sound.rate,
            refresh_ns: context.refresh_ns,
        };
        let place = move |ahead: usize| board.lock().show.sound_place(&clock, sys::monotonic_ns(), ahead);
        sound::play_along(file, place).inspect_err(|e| log!("gallery: {name}: no sound: {e}")).ok()
    }

    fn rewind(&mut self) {
        for buffer in self.feed.rewind() {
            self.decoder.give_back(buffer);
        }
        self.decoder.rewind();
    }

    fn fetch(&mut self) -> io::Result<()> {
        if let Some(number) = self.feed.next_unread(self.video.movie.samples.len()) {
            self.video.sample(number, &mut self.sample)?;
            self.feed.mark_read();
        }
        Ok(())
    }

    fn begin(&mut self) -> io::Result<()> {
        self.fetch()?;
        let Some(number) = self.feed.ready_sample() else {
            if self.feed.note_end() {
                self.feed.add_pictures(self.decoder.drain());
            }
            return Ok(());
        };
        let units: Vec<&[u8]> = mp4::units(&self.sample, self.video.movie.length_bytes).collect();
        let outcome = self.decoder.begin(&units, number as u64)?;
        let start = self.feed.take_outcome(outcome);
        if let SampleStart::Skipped { is_first } = start {
            if is_first {
                log!("{}", feed::to_skip_line(number, outcome));
            }
            self.stats.add_skipped();
        }
        Ok(())
    }

    fn end(&mut self) -> io::Result<()> {
        let outcome = self.decoder.end()?;
        self.stats.add_decoded(outcome.is_flawed(), outcome.lock_micros);
        self.feed.add_pictures(outcome.outputs);
        Ok(())
    }

    fn paint(&mut self, buffer: usize, picture: &mut Picture) {
        let (start, cpu_start) = (Instant::now(), sys::cpu_ns());
        project(&self.decoders.crew, &self.projector, self.decoder.picture(buffer), &mut self.scratch, picture);
        self.stats.add_drawn(start.elapsed().as_nanos() as u64, sys::cpu_ns().saturating_sub(cpu_start));
    }

    fn draw(&mut self, picture: &mut Picture) -> Option<u64> {
        let (next, previous) = self.feed.take_next()?;
        self.paint(next.buffer, picture);
        if let Some(previous) = previous {
            self.decoder.give_back(previous);
        }
        Some(due_ns(self.video.movie.time_of(next.stamp as usize), self.scale))
    }

    fn redraw_last(&mut self, picture: &mut Picture) -> bool {
        let Some(buffer) = self.feed.last_buffer() else {
            return false;
        };
        self.paint(buffer, picture);
        true
    }

    fn first_picture(&mut self, picture: &mut Picture) -> io::Result<()> {
        loop {
            match self.feed.first_step() {
                FirstStep::Begin => self.begin()?,
                FirstStep::End => self.end()?,
                FirstStep::Full => return Err(io::Error::other(NO_BUFFER)),
                FirstStep::Draw => return self.draw(picture).map(drop).ok_or_else(|| sys::invalid(NO_PICTURE)),
            }
        }
    }

    fn step(&mut self, shared: &Shared, count: usize) -> io::Result<()> {
        self.feed.begin_round(count);
        loop {
            match self.feed.next_step() {
                DrawStep::Begin => self.begin()?,
                DrawStep::Draw => {
                    let mut picture = shared.board.lock().show.take_blank();
                    if let Some(due_ns) = self.draw(&mut picture) {
                        shared.board.lock().show.add(picture, due_ns);
                    }
                }
                DrawStep::End => {
                    let result = self.fetch();
                    self.end()?;
                    result?;
                }
                DrawStep::Rest => return Ok(()),
            }
        }
    }

    fn close(self) {
        self.decoders.keep_video(self.decoder);
    }
}

pub(super) fn bytes_of(data: &Data, name: &str) -> io::Result<u64> {
    Ok(data.open(VIDEOS, name)?.metadata()?.len())
}

pub(super) fn read_poster(context: Context, name: &str, place: (usize, usize)) -> io::Result<Thumbnail> {
    let mut playback = Playback::open(context, name, |sequence, quarters| {
        projection::poster_projector(sequence, quarters, context.format, place)
    })?;
    let mut picture = Picture::default();
    let result = playback.first_picture(&mut picture);
    let movie = &playback.video.movie;
    let thumbnail = result.map(|()| Thumbnail {
        picture,
        label: Label::Seconds(movie.seconds()),
        size: upright_size(&playback.sequence, movie.quarters),
        file: name.into(),
    });
    playback.close();
    thumbnail
}

fn show_video(shared: &Shared, context: Context, name: &str, at: usize, request: RollRequest) -> io::Result<()> {
    let pose = shared.board.lock().pose;
    let mut playback = Playback::open(context, name, |sequence, quarters| {
        projection::screen_projector(sequence, quarters, context.format, request.room, pose)
    })?;
    let result = run_playback(shared, context, &mut playback, name, at, request, pose);
    playback.close();
    result
}

// Ends the playing: the speaker is let go of, the newest picture stands for the screen, and the log tells how the playing went.
fn stop(shared: &Shared, playback: &mut Playback, name: &str, at: usize, speaker: &mut Option<SoundStream>) {
    drop(speaker.take());
    let clock = shared.hold((at, name));
    log!("{}", playback.stats.take_log_line(name, clock));
}

fn run_playback(
    shared: &Shared,
    context: Context,
    playback: &mut Playback,
    name: &str,
    at: usize,
    request: RollRequest,
    pose: Pose,
) -> io::Result<()> {
    let mut picture = Picture::default();
    playback.first_picture(&mut picture)?;
    shared.show(&mut picture, Kind::Video, (at, name));
    log!("{}", playback.to_shown_line(name, pose));
    let mut player = Player::new(pose);
    // The speaker, while the video plays and its file has sound: let go of whenever the pictures stop.
    let mut speaker: Option<SoundStream> = None;
    loop {
        let ask = shared.board.lock().take_ask(request);
        player.begin_pass(ask);
        loop {
            match player.next_step(playback.feed.is_done()) {
                PlayStep::Leave(is_playing) => {
                    if is_playing {
                        stop(shared, playback, name, at, &mut speaker);
                    }
                    return Ok(());
                }
                PlayStep::Turn(needs_redraw) => {
                    playback.turn(request.room, player.pose)?;
                    log!("{}", playback.to_shown_line(name, player.pose));
                    if needs_redraw && playback.redraw_last(&mut picture) {
                        shared.show(&mut picture, Kind::Video, (at, name));
                    }
                }
                PlayStep::Play(needs_rewind) => {
                    if needs_rewind {
                        playback.rewind();
                    }
                    shared.play();
                    speaker = playback.play_sound(context, name, shared);
                }
                PlayStep::Hold => stop(shared, playback, name, at, &mut speaker),
                PlayStep::Await => break,
            }
        }
        let is_done = playback.feed.is_done();
        match shared.board.wait(|board| board.next_step(request, &player, is_done)) {
            VideoStep::Request => {}
            VideoStep::End => {
                player.note_end();
                stop(shared, playback, name, at, &mut speaker);
            }
            VideoStep::Draw(count) => playback.step(shared, count)?,
        }
    }
}

pub(super) fn show_videos(shared: &Shared, context: Context, items: &[Item]) {
    while let Some((request, at)) = shared.open_item(Kind::Video, items.len()) {
        let name = &items[at].original;
        if let Err(e) = show_video(shared, context, name, at, request) {
            log!("gallery: {name}: {e}");
            shared.clear();
            shared.show_unreadable(Kind::Video, (at, name));
            shared.board.wait(|board| board.drop_clicks(request).then_some(()));
        }
        shared.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{FORMAT, ROOM, word_picture};
    use super::*;
    use r1_funcore::codec::h264::read::Order;
    use r1_funcore::hw::vdec::Layout;

    fn stream(columns: u32, rows: u32, crop: u32) -> Sequence {
        Sequence {
            id: 0,
            profile: 100,
            level: 41,
            third_constraint: false,
            number_bits: 4,
            order: Order::Frame,
            references: 1,
            columns,
            rows,
            direct_8x8: true,
            crop: [0, 0, 0, crop],
            is_full_range: false,
            matrix: Some(1),
            reorder: None,
            buffering: None,
        }
    }

    fn to_frame(sequence: &Sequence) -> Vec<u8> {
        let layout = Layout::from_sequence(sequence).unwrap();
        (0..layout.picture_bytes()).map(|at| (at % 251 + at / 4096) as u8).collect()
    }

    #[test]
    fn crew_projection_equals_one_thread() {
        let recording = stream(80, 45, 0);
        let (frame, crew) = (to_frame(&recording), Crew::new("test", FRAME_PARTS - 1).unwrap());
        let empty_crew = Crew::new("test", 0).unwrap();
        let mut scratch: [Scratch; FRAME_PARTS] = Default::default();
        for (quarters, pose) in [(1, Pose::Upright), (1, Pose::RightUp), (0, Pose::UpsideDown), (2, Pose::LeftUp)] {
            let projector = projector(&recording, quarters, FORMAT, ROOM, pose).unwrap();
            let (width, height) = projector.size();
            let mut expected = vec![0u32; width * height];
            projector.draw(&frame, 0..height, &mut expected, &mut Scratch::default());
            for crew in [&crew, &empty_crew] {
                let mut picture = word_picture(5);
                project(crew, &projector, &frame, &mut scratch, &mut picture);
                assert_eq!(
                    (picture.width, picture.words == expected),
                    (width, true),
                    "{quarters} quarter turns in the file, held {pose:?}"
                );
            }
        }
        let small = stream(4, 4, 62);
        let projector = projector(&small, 0, FORMAT, ROOM, Pose::Upright).unwrap();
        let mut picture = Picture::default();
        project(&crew, &projector, &to_frame(&small), &mut scratch, &mut picture);
        assert_eq!(
            (projector.size(), picture.width, picture.words.len()),
            ((64, 2), 64, 128),
            "fewer lines than threads"
        );
    }
}
