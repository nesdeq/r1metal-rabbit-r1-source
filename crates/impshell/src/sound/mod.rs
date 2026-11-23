mod memo;
mod mixer;
mod pcm;

pub use memo::Memo;

use crate::log;
use crate::media::recording::SoundFile;
use crate::media::reel::{Reel, SoundSender, Summary};
use crate::pool::ThreadPool;
use crate::sync::Board;
use crate::sys::{self, Wake};
use mixer::Mixer;
use pcm::Pcm;
use r1_funcore::codec::Flaw;
use r1_funcore::codec::aac::RATE;
use r1_funcore::codec::mp4::{Content, Soundtrack};
use r1_funcore::hw::sound::card::{self, CardState, Direction, Gain};
use r1_funcore::hw::sound::pace::Follower;
use r1_funcore::hw::sound::playback::{MemoryFile, Packets, Playback, UNSUPPORTED, to_undecoded_line};
use r1_funcore::hw::sound::record::{Lineup, Outlet, VOICE_BITS_PER_SECOND, sound_format};
use r1_funcore::hw::sound::volume;
use r1_funcore::hw::sound::{Intake, SPEAKER_BUSY_LINE, peak, to_lost_line};
use std::fs::File;
use std::io;
use std::os::fd::RawFd;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Instant;

const BLOCK: usize = card::PERIOD;

// The card's state, which every thread of the card and the main loop share: its flag tells of the microphone's level.
static CARD: LazyLock<Board<CardState>> =
    LazyLock::new(|| Board::new(CardState::default()).expect("a flag for the sound card's news"));
static MICROPHONE_POOL: ThreadPool = ThreadPool::new("microphone");
static SPEAKER_POOL: ThreadPool = ThreadPool::new("speaker");

/// What the microphone's thread hands a capture.
pub enum Capture<'a> {
    Start(Instant),
    // A block, and the samples recorded after it that wait to be read, as the card told them just now.
    Samples(&'a [i16], usize),
    // Samples the thread came too late for.
    Gap(usize),
    End,
}

/// What the speaker's thread hands a player that hears what it plays.
pub enum Played<'a> {
    // A block as the card takes it, at its volume.
    Block(&'a [i16]),
    // The card ran dry before the block it was last handed.
    Underrun,
}

/// A running microphone or speaker; dropping it stops it.
pub struct SoundStream {
    stop: Arc<Wake>,
}

impl Drop for SoundStream {
    fn drop(&mut self) {
        self.stop.raise();
    }
}

/// True while neither the microphone nor the speaker is held and no sound waits for the speaker.
pub fn is_off() -> bool {
    CARD.lock().is_quiet()
}

/// Readable when the microphone's level has news.
pub fn level_fd() -> RawFd {
    CARD.fd()
}

/// Takes the largest sample of the block the microphone recorded last, nought while it records nothing.
pub fn take_peak() -> u32 {
    CARD.clear();
    CARD.lock().peak()
}

fn read_blocks(rate: u32, stop: &Wake, on_capture: &mut impl FnMut(Capture)) -> io::Result<()> {
    let pcm = Pcm::open(Direction::In, rate, card::periods(Direction::In))?;
    pcm.start()?;
    let start = Instant::now();
    on_capture(Capture::Start(start));
    let (mut block, mut intake) = ([0i16; BLOCK], Intake::new(rate));
    while !stop.is_raised() {
        // An overrun the delay's call meets is one the read met: the block read before it goes into the gap.
        match pcm.read(&mut block).and_then(|()| pcm.delay()) {
            Ok(delay) => {
                intake.add(BLOCK as u64);
                // A block whose card state another thread holds tells no level, as the next one does: a call's thread waits on no lower one.
                if CARD.try_lock().is_some_and(|mut card| card.hear(u32::from(peak(&block)))) {
                    CARD.raise();
                }
                on_capture(Capture::Samples(&block, delay));
            }
            Err(e) if pcm::is_xrun(&e) => {
                pcm.prepare()?;
                pcm.start()?;
                on_capture(Capture::Gap(intake.take_lost(start.elapsed()) as usize));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Starts the microphone on its thread and hands `on_capture` what it records, until the stream is dropped.
pub fn capture(rate: u32, mut on_capture: impl FnMut(Capture) + Send + 'static) -> io::Result<SoundStream> {
    let stop = Arc::new(Wake::new()?);
    let worker_stop = Arc::clone(&stop);
    MICROPHONE_POOL.run(move || {
        CARD.wait(CardState::take_microphone);
        // A capture let go of while it waited for the microphone switches nothing on.
        if !worker_stop.is_raised() {
            let result = Mixer::open().and_then(|mixer| {
                let result =
                    mixer.set(card::microphone(true)).and_then(|()| read_blocks(rate, &worker_stop, &mut on_capture));
                mixer.set(card::microphone(false)).and(result)
            });
            if let Err(e) = result {
                log!("sound: the microphone: {e}");
            }
        }
        on_capture(Capture::End);
        CARD.lock().free_microphone();
        CARD.notify();
        CARD.raise();
    })?;
    Ok(SoundStream { stop })
}

/// A recording's sound on its way from the microphone into its file.
pub struct Recording {
    _capture: SoundStream,
    file_start: Arc<OnceLock<Instant>>,
}

impl Outlet for SoundSender {
    fn buffer(&mut self) -> Vec<u8> {
        self.empty_buffer()
    }

    fn send(&mut self, packet: Vec<u8>) {
        // A file that takes no more is told by whoever writes its frames, or by its own end.
        let _ = self.send_packet(packet);
    }
}

struct CaptureSink {
    lineup: Lineup,
    sender: SoundSender,
    file_start: Arc<OnceLock<Instant>>,
    start: Option<Instant>,
}

impl CaptureSink {
    fn take(&mut self, capture: Capture) {
        match capture {
            Capture::Start(start) => self.start = Some(start),
            Capture::Samples(samples, _) => {
                let offsets = self.file_start.get().zip(self.start).map(|(&file_start, start)| {
                    (start.saturating_duration_since(file_start), file_start.saturating_duration_since(start))
                });
                self.lineup.take(samples, offsets, &mut self.sender);
            }
            Capture::Gap(samples) => {
                log!("{}", to_lost_line(samples as u64));
                self.lineup.add_lost(samples, &mut self.sender);
            }
            Capture::End => self.lineup.end(&mut self.sender),
        }
    }
}

/// Starts the microphone for a recording; packets go to `sender` from the moment `begin` names.
pub fn record(sender: SoundSender, bits_per_second: u32) -> io::Result<Recording> {
    let file_start = Arc::new(OnceLock::new());
    let mut sink =
        CaptureSink { lineup: Lineup::new(bits_per_second), sender, file_start: Arc::clone(&file_start), start: None };
    let stream = capture(RATE, move |capture| sink.take(capture))?;
    Ok(Recording { _capture: stream, file_start })
}

impl Recording {
    /// Names the moment the file begins; silence fills a microphone that began later.
    pub fn begin(&self, at: Instant) {
        let _ = self.file_start.set(at);
    }
}

/// A voice being recorded into a file.
pub struct VoiceRecording {
    reel: Reel,
    recording: Recording,
}

impl VoiceRecording {
    /// Ends the recording and finishes the file; none if nothing was heard.
    pub fn end(self) -> io::Result<Option<Summary>> {
        drop(self.recording);
        self.reel.close().finish()
    }
}

/// Begins recording a voice into the file `create` makes.
pub fn record_voice(
    create: impl FnOnce() -> io::Result<(File, String)> + Send + 'static,
) -> io::Result<VoiceRecording> {
    let (reel, sender) = Reel::start(create, Content::Sound(sound_format(), sys::realtime_seconds()))?;
    let recording = record(sender, VOICE_BITS_PER_SECOND)?;
    recording.begin(Instant::now());
    Ok(VoiceRecording { reel, recording })
}

/// Sets the speaker's volume and the interface sounds' level in percent; what plays follows with its next block.
pub fn set_volume(volume: u8, sounds: u8) {
    CARD.lock().set_levels(volume, sounds);
}

// What fills the speaker's blocks, each with the card's delay before its first sample is heard, and what takes each block as the card plays it, at its volume, and each underrun.
struct Feed<N, H> {
    next: N,
    heard: H,
}

fn write_blocks(
    stop: &Wake,
    pcm: &Pcm,
    gain: Gain,
    feed: &mut Feed<impl FnMut(&mut [i16], usize) -> bool, impl FnMut(Played)>,
) -> io::Result<()> {
    let (mut block, mut level) = ([0i16; BLOCK], CARD.lock().gain(gain));
    loop {
        // An underrun the delay's call meets is one the write would meet: the stream is made ready, and the block stamped by the delay of a ring that holds nothing.
        let delay = match pcm.delay() {
            Err(e) if pcm::is_xrun(&e) => {
                (feed.heard)(Played::Underrun);
                pcm.prepare()?;
                pcm.delay()?
            }
            result => result?,
        };
        if stop.is_raised() || !(feed.next)(&mut block, delay) {
            break;
        }
        // A block whose card state another thread holds plays at the gain the last did: a call's thread waits on no lower one.
        if let Some(card) = CARD.try_lock() {
            level = card.gain(gain);
        }
        volume::scale(&mut block, level);
        (feed.heard)(Played::Block(&block));
        match pcm.write(&block) {
            Err(e) if pcm::is_xrun(&e) => {
                (feed.heard)(Played::Underrun);
                pcm.prepare()?;
                pcm.write(&block)?;
            }
            result => result?,
        }
    }
    if stop.is_raised() { Ok(()) } else { pcm.drain() }
}

fn run_speaker(
    stop: &Wake,
    gain: Gain,
    periods: usize,
    feed: &mut Feed<impl FnMut(&mut [i16], usize) -> bool, impl FnMut(Played)>,
) {
    if stop.is_raised() {
        return;
    }
    let result = Mixer::open().and_then(|mixer| {
        let pcm = Pcm::open(Direction::Out, RATE, periods)?;
        let result = mixer.set(card::speaker(true)).and_then(|()| write_blocks(stop, &pcm, gain, feed));
        // The amplifier goes off before the stream under it ends.
        mixer.set(card::speaker(false)).and(result)
    });
    if let Err(e) = result {
        log!("sound: the speaker: {e}");
    }
}

// The main loop is told too: asleep, it waits for a sound's end to suspend.
fn free_speaker() {
    CARD.lock().free_speaker();
    CARD.notify();
    CARD.raise();
}

fn play_at(
    gain: Gain,
    periods: usize,
    mut feed: Feed<impl FnMut(&mut [i16], usize) -> bool + Send + 'static, impl FnMut(Played) + Send + 'static>,
    on_end: impl FnOnce() + Send + 'static,
) -> io::Result<SoundStream> {
    let stop = Arc::new(Wake::new()?);
    let worker_stop = Arc::clone(&stop);
    // Counted as waiting before its thread runs, so that no suspend comes between the asking and the sound.
    CARD.lock().await_speaker();
    let job = SPEAKER_POOL.run(move || {
        CARD.wait(CardState::take_awaited_speaker);
        run_speaker(&worker_stop, gain, periods, &mut feed);
        free_speaker();
        on_end();
    });
    if job.is_err() {
        CARD.lock().stop_awaiting();
    }
    job?;
    Ok(SoundStream { stop })
}

/// Plays the blocks `next` fills at the volume once the speaker is free, until `next` has no more or the stream is dropped; then `on_end` runs.
pub fn play(
    next: impl FnMut(&mut [i16], usize) -> bool + Send + 'static,
    on_end: impl FnOnce() + Send + 'static,
) -> io::Result<SoundStream> {
    play_at(Gain::Volume, card::periods(Direction::Out), Feed { next, heard: |_: Played| {} }, on_end)
}

/// Plays as `play` does in a call's shorter ring, and hands `heard` each block as the card plays it, at its volume, and each underrun: what a call clears its microphone's sound of.
pub fn play_heard(
    next: impl FnMut(&mut [i16], usize) -> bool + Send + 'static,
    heard: impl FnMut(Played) + Send + 'static,
) -> io::Result<SoundStream> {
    play_at(Gain::Volume, card::CALL_PERIODS, Feed { next, heard }, || {})
}

/// Plays an interface sound, the blocks `next` fills, once the speaker is free, until `next` has no more or the stream is dropped.
pub fn play_sound(mut next: impl FnMut(&mut [i16]) -> bool + Send + 'static) -> io::Result<SoundStream> {
    let feed = Feed { next: move |block: &mut [i16], _| next(block), heard: |_: Played| {} };
    play_at(Gain::Sounds, card::periods(Direction::Out), feed, || {})
}

/// Plays an interface sound only if the speaker is free; a waiter takes the speaker once the card has played out.
pub fn play_aside(mut next: impl FnMut(&mut [i16]) -> bool + Send + 'static) -> io::Result<SoundStream> {
    let stop = Arc::new(Wake::new()?);
    let worker_stop = Arc::clone(&stop);
    SPEAKER_POOL.run(move || {
        if !CARD.lock().take_speaker_aside() {
            return log!("{SPEAKER_BUSY_LINE}");
        }
        let next = |block: &mut [i16], _| CARD.lock().may_keep_speaker() && next(block);
        run_speaker(
            &worker_stop,
            Gain::Sounds,
            card::periods(Direction::Out),
            &mut Feed { next, heard: |_: Played| {} },
        );
        free_speaker();
    })?;
    Ok(SoundStream { stop })
}

impl Packets for SoundFile {
    fn index(&self) -> &Soundtrack {
        &self.track
    }

    fn read(&mut self, number: usize, bytes: &mut Vec<u8>) -> bool {
        self.packet(number, bytes).inspect_err(|e| log!("sound: packet {number} of what is played: {e}")).is_ok()
    }

    fn note_flaw(&mut self, number: usize, flaw: Flaw) {
        log!("{}", to_undecoded_line(number, flaw));
    }
}

fn playback<P: Packets>(file: P) -> io::Result<Playback<P>> {
    Playback::new(file).ok_or_else(|| sys::invalid(UNSUPPORTED))
}

/// Plays a sound file from its start; `on_end` runs when it has, or the stream was dropped.
pub fn play_file(file: SoundFile, on_end: impl FnOnce() + Send + 'static) -> io::Result<SoundStream> {
    let mut playback = playback(file)?;
    play(move |block, _| playback.fill_block(block), on_end)
}

// A voice note held whole in memory.
struct Voice(MemoryFile<'static>);

impl Packets for Voice {
    fn index(&self) -> &Soundtrack {
        self.0.index()
    }

    fn read(&mut self, number: usize, bytes: &mut Vec<u8>) -> bool {
        self.0.read(number, bytes)
    }

    fn note_flaw(&mut self, number: usize, flaw: Flaw) {
        log!("{}", to_undecoded_line(number, flaw));
    }
}

/// Plays a voice note from its bytes, an MP4 file or a stream of ADTS, from its start; `on_end` runs when it has, or the stream was dropped.
pub fn play_voice(bytes: Vec<u8>, on_end: impl FnOnce() + Send + 'static) -> io::Result<SoundStream> {
    let note = MemoryFile::parse(bytes).map_err(|flaw| sys::invalid(flaw.to_string()))?;
    let mut playback = playback(Voice(note))?;
    play(move |block, _| playback.fill_block(block), on_end)
}

/// Plays a file's sound along a video's clock: `place` gives, from the card's delay, the sample due; none while the clock stands.
pub fn play_along(
    file: SoundFile,
    mut place: impl FnMut(usize) -> Option<f64> + Send + 'static,
) -> io::Result<SoundStream> {
    let (mut playback, mut follower) = (playback(file)?, Follower::default());
    play(
        move |block: &mut [i16], delay: usize| {
            follower.follow(place(delay), &mut playback, block);
            true
        },
        || {},
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::codec::aac::{DELAY, Decoder, FRAME};
    use r1_funcore::hw::sound::pace::Source;
    use r1_funcore::hw::sound::record::SETTLE_SAMPLES;
    use std::fs::{self, File};
    use std::time::Duration;

    const BITS: u32 = 96_000;

    fn sink(name: &str) -> (CaptureSink, crate::media::reel::PendingFile, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("r1-lane-{name}-{}.m4a", std::process::id()));
        let file_path = path.clone();
        let (reel, sender) =
            Reel::start(move || Ok((File::create(file_path)?, "note.m4a".into())), Content::Sound(sound_format(), 0))
                .unwrap();
        let sink =
            CaptureSink { lineup: Lineup::new(BITS), sender, file_start: Arc::new(OnceLock::new()), start: None };
        (sink, reel.close(), path)
    }

    fn decode(path: &std::path::Path) -> Vec<i16> {
        let file = SoundFile::open(File::open(path).unwrap()).unwrap().unwrap();
        let (mut decoder, mut packet, mut samples) = (Decoder::new(), Vec::new(), Vec::new());
        for number in 0..file.track.packets.len() {
            let mut frame = [0i16; FRAME];
            file.packet(number, &mut packet).unwrap();
            decoder.decode(&packet, &mut frame).unwrap();
            samples.extend_from_slice(&frame);
        }
        samples.split_off(file.track.sound.lead as usize)
    }

    fn hum(samples: usize) -> Vec<i16> {
        (0..samples).map(|at| if at / 60 % 2 == 0 { 3000 } else { -3000 }).collect()
    }

    fn is_loud(samples: &[i16]) -> bool {
        samples.iter().any(|sample| sample.unsigned_abs() > 1000)
    }

    #[test]
    fn late_microphone_gets_silence() {
        let (mut sink, pending, path) = sink("late");
        let file_start = Instant::now() - Duration::from_millis(500);
        sink.file_start.set(file_start).unwrap();
        sink.take(Capture::Start(file_start + Duration::from_millis(100)));
        for _ in 0..30 {
            sink.take(Capture::Samples(&hum(BLOCK), 0));
        }
        sink.take(Capture::End);
        drop(sink);
        assert_eq!(
            pending.finish().unwrap().unwrap().packets,
            36,
            "30 blocks behind 4800 samples of silence, and one to flush"
        );
        let samples = decode(&path);
        let sound_at = 4800 + SETTLE_SAMPLES;
        assert!(!is_loud(&samples[..sound_at - 1100]) && is_loud(&samples[sound_at + 1100..sound_at + 4000]));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn early_sound_left_out() {
        let (mut sink, pending, path) = sink("early");
        let start = Instant::now() - Duration::from_secs(2);
        sink.take(Capture::Start(start));
        for _ in 0..10 {
            sink.take(Capture::Samples(&hum(BLOCK), 0));
        }
        sink.take(Capture::Gap(BLOCK));
        sink.file_start.set(start + Duration::from_millis(250)).unwrap();
        for _ in 0..20 {
            sink.take(Capture::Samples(&hum(BLOCK), 0));
        }
        sink.take(Capture::Gap(2 * BLOCK));
        sink.take(Capture::End);
        drop(sink);
        let packets = pending.finish().unwrap().unwrap().packets;
        assert_eq!(
            packets,
            22 + 1,
            "of 12000 samples before the file 736 were still to leave out; what was lost later is silence"
        );
        assert_eq!(decode(&path).len(), packets * FRAME - DELAY);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn seek_matches_decode() {
        let (mut sink, pending, path) = sink("seek");
        let start = Instant::now();
        sink.file_start.set(start).unwrap();
        sink.take(Capture::Start(start));
        for _ in 0..40 {
            sink.take(Capture::Samples(&hum(BLOCK), 0));
        }
        sink.take(Capture::End);
        drop(sink);
        pending.finish().unwrap().unwrap();
        let samples = decode(&path);
        let mut playback = playback(SoundFile::open(File::open(&path).unwrap()).unwrap().unwrap()).unwrap();
        for first in [0usize, 1, FRAME - 1, FRAME, 5 * FRAME + 300, 20_000] {
            playback.seek(first as u64);
            let part: Vec<i16> = (0..2 * FRAME).map(|_| Source::next(&mut playback)).collect();
            assert_eq!(part, samples[first..first + 2 * FRAME], "from sample {first}");
        }
        playback.seek(samples.len() as u64 + 5 * FRAME as u64);
        assert_eq!(Source::next(&mut playback), 0, "behind the last sample there is silence");
        fs::remove_file(path).unwrap();
    }
}
