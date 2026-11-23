use crate::log;
use crate::pool::{JobHandle, ThreadPool};
use r1_funcore::codec::mp4::{BACKLOG, Content};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

static WRITERS: ThreadPool = ThreadPool::new("recording");

enum Piece {
    Frame { bytes: Vec<u8>, is_key: bool, time: u64 },
    Packet(Vec<u8>),
}

/// A finished file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Summary {
    pub name: String,
    pub samples: usize,
    pub packets: usize,
    pub bytes: u64,
    /// Ticks of the video clock from the first frame to the end of the last.
    pub ticks: u64,
}

trait Medium: Write + Seek {
    fn truncate(&mut self, length: u64) -> io::Result<()>;
}

impl Medium for File {
    fn truncate(&mut self, length: u64) -> io::Result<()> {
        self.set_len(length)
    }
}

fn write<M: Medium>(
    medium: &mut M,
    name: String,
    content: Content,
    pieces: impl Iterator<Item = Piece>,
    mut give_back: impl FnMut(Vec<u8>, bool),
) -> io::Result<Summary> {
    let (mut writer, head) = content.begin();
    medium.write_all(&head)?;
    for piece in pieces {
        let bytes = match &piece {
            Piece::Frame { bytes, .. } | Piece::Packet(bytes) => bytes,
        };
        if let Err(e) = medium.write_all(bytes) {
            log!("recording: the file takes no more: {e}");
            medium.truncate(writer.length())?;
            medium.seek(SeekFrom::Start(writer.length()))?;
            break;
        }
        match piece {
            Piece::Frame { bytes, is_key, time } => {
                writer.add_sample(bytes.len() as u32, is_key, time);
                give_back(bytes, false);
            }
            Piece::Packet(bytes) => {
                writer.add_packet(bytes.len() as u32);
                give_back(bytes, true);
            }
        }
    }
    let (samples, packets, ticks) = (writer.sample_count(), writer.packet_count(), writer.ticks());
    let tail = writer.finish();
    medium.write_all(&tail.index)?;
    let bytes = medium.stream_position()?;
    medium.seek(SeekFrom::Start(tail.size_at))?;
    medium.write_all(&tail.size)?;
    Ok(Summary { name, samples, packets, bytes, ticks })
}

/// A file being written; takes the frames.
pub struct Reel {
    pieces: SyncSender<Piece>,
    spare: Receiver<Vec<u8>>,
    job: JobHandle<io::Result<Option<Summary>>>,
}

/// The side of a reel that takes the sound's packets.
pub struct SoundSender {
    pieces: SyncSender<Piece>,
    spare: Receiver<Vec<u8>>,
}

/// A file whose last pieces and index are still on their way.
pub struct PendingFile {
    job: JobHandle<io::Result<Option<Summary>>>,
}

fn empty_buffer(spare: &Receiver<Vec<u8>>) -> Vec<u8> {
    let mut bytes = spare.try_recv().unwrap_or_default();
    bytes.clear();
    bytes
}

impl Reel {
    /// Starts writing a recording on a writer thread; `open` makes the file and its name at the first frame or, for sound alone, packet.
    pub fn start(
        open: impl FnOnce() -> io::Result<(File, String)> + Send + 'static,
        content: Content,
    ) -> io::Result<(Reel, SoundSender)> {
        let (pieces, receiver) = sync_channel(BACKLOG);
        let (frames_back, frame_spare) = sync_channel(BACKLOG);
        let (packets_back, packet_spare) = sync_channel(BACKLOG);
        let sender = SoundSender { pieces: pieces.clone(), spare: packet_spare };
        let job = WRITERS.run(move || {
            let (mut pending, mut has_start) = (Vec::new(), false);
            for piece in receiver.iter() {
                has_start = content.starts_file(matches!(piece, Piece::Frame { .. }));
                pending.push(piece);
                if has_start {
                    break;
                }
            }
            if !has_start {
                return Ok(None);
            }
            let (mut file, name) = open()?;
            let pieces = pending.into_iter().chain(receiver);
            let summary = write(&mut file, name, content, pieces, |bytes, is_sound| {
                drop(if is_sound { packets_back.try_send(bytes) } else { frames_back.try_send(bytes) })
            })?;
            file.sync_all()?;
            Ok(Some(summary))
        })?;
        Ok((Reel { pieces, spare: frame_spare, job }, sender))
    }

    /// An empty buffer for a frame's bytes.
    pub fn empty_buffer(&self) -> Vec<u8> {
        empty_buffer(&self.spare)
    }

    /// Hands the thread a frame, waiting while it is `BACKLOG` pieces behind; fails once it has given up on the file.
    pub fn send_frame(&self, bytes: Vec<u8>, is_key: bool, time: u64) -> io::Result<()> {
        self.pieces
            .send(Piece::Frame { bytes, is_key, time })
            .map_err(|_| io::Error::other("the file takes no more frames"))
    }

    /// Ends the frames; the file is complete once its sound has ended too.
    pub fn close(self) -> PendingFile {
        PendingFile { job: self.job }
    }
}

impl SoundSender {
    /// An empty buffer for a packet's bytes.
    pub fn empty_buffer(&self) -> Vec<u8> {
        empty_buffer(&self.spare)
    }

    /// Hands the thread a packet, waiting while it is `BACKLOG` pieces behind; fails once it has given up on the file.
    pub fn send_packet(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.pieces.send(Piece::Packet(bytes)).map_err(|_| io::Error::other("the file takes no more sound"))
    }
}

impl PendingFile {
    /// True once the file is complete or given up.
    pub fn is_done(&self) -> bool {
        self.job.is_finished()
    }

    /// Waits until the file is complete and on the medium; none if what begins a file never came.
    pub fn finish(self) -> io::Result<Option<Summary>> {
        self.job.join().unwrap_or_else(|| Err(io::Error::other("the thread that writes the recording died")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::codec::mp4::{Sound, VideoTrack};
    use std::fs::{self, OpenOptions};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::thread;

    const FRAME: u64 = 7999;
    const HEAD: usize = 48;

    fn track() -> VideoTrack {
        VideoTrack {
            width: 1280,
            height: 720,
            ticks_per_second: 480_000,
            ticks_per_frame: FRAME as u32,
            sequence_parameters: vec![0x67, 0x64, 0x00, 0x29],
            picture_parameters: vec![0x68, 0xEA],
            quarters: 1,
            creation_time: 1_791_500_000,
        }
    }

    fn sound() -> Sound {
        Sound { rate: 48_000, frame: 1024, channels: 1, configuration: vec![0x11, 0x88], lead: 1024 }
    }

    fn word(bytes: &[u8], at: usize) -> usize {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    }

    fn scratch(name: &str, file_name: &'static str) -> (PathBuf, impl FnOnce() -> io::Result<(File, String)> + Send) {
        let path = std::env::temp_dir().join(format!("r1-reel-{name}-{}.mp4", std::process::id()));
        let file_path = path.clone();
        (path, move || Ok((File::create(file_path)?, file_name.into())))
    }

    fn frame(bytes: Vec<u8>, is_key: bool, time: u64) -> Piece {
        Piece::Frame { bytes, is_key, time }
    }

    struct Tape {
        bytes: Cursor<Vec<u8>>,
        writes: usize,
        failing_write: usize,
    }

    impl Write for Tape {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.writes == self.failing_write {
                self.bytes.write_all(&bytes[..bytes.len() / 2])?;
                return Err(io::ErrorKind::StorageFull.into());
            }
            self.bytes.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for Tape {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.bytes.seek(to)
        }
    }

    impl Medium for Tape {
        fn truncate(&mut self, length: u64) -> io::Result<()> {
            self.bytes.get_mut().truncate(length as usize);
            Ok(())
        }
    }

    #[test]
    fn write_head_samples_index() {
        let (path, open) = scratch("whole", "clip.mp4");
        let (reel, sender) = Reel::start(open, Content::Video(track(), sound())).unwrap();
        // A recording whose microphone gave nothing: the end for its sound is let go of unused.
        drop(sender);
        for (index, length) in [1000usize, 200, 300, 900].into_iter().enumerate() {
            reel.send_frame(vec![index as u8 + 1; length], index % 3 == 0, index as u64 * FRAME).unwrap();
        }
        let mut buffer = reel.empty_buffer();
        while buffer.capacity() < 200 {
            thread::yield_now();
            buffer = reel.empty_buffer();
        }
        assert!(buffer.is_empty(), "what held a written sample comes back empty, to hold another");
        let summary = reel.close().finish().unwrap();
        let file = fs::read(&path).unwrap();
        assert_eq!(
            summary,
            Some(Summary {
                name: "clip.mp4".into(),
                samples: 4,
                packets: 0,
                bytes: file.len() as u64,
                ticks: 4 * FRAME
            })
        );
        assert_eq!((&file[4..8], &file[36..40]), (&b"ftyp"[..], &b"mdat"[..]));
        assert_eq!(
            (word(&file, 32), word(&file, 40), word(&file, 44)),
            (1, 0, 16 + 2400),
            "the size of the samples' box stands behind its name"
        );
        assert_eq!(
            (file[HEAD], file[HEAD + 999], file[HEAD + 1000], file[HEAD + 2399]),
            (1, 1, 2, 4),
            "the samples lie one after another behind the head"
        );
        assert_eq!(&file[HEAD + 2404..HEAD + 2408], b"moov");
        assert_eq!(word(&file, HEAD + 2400), file.len() - HEAD - 2400, "the index is the rest of the file");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn no_sample_no_file() {
        let (path, open) = scratch("none", "clip.mp4");
        let (reel, sender) = Reel::start(open, Content::Video(track(), sound())).unwrap();
        sender.send_packet(vec![7; 50]).unwrap();
        drop(sender);
        assert_eq!(reel.close().finish().unwrap(), None, "sound without a frame is no recording of video");
        assert!(!path.exists());
    }

    #[test]
    fn sound_between_frames() {
        let (path, open) = scratch("sound", "clip.mp4");
        let (reel, sender) = Reel::start(open, Content::Video(track(), sound())).unwrap();
        sender.send_packet(vec![0xA1; 50]).unwrap();
        reel.send_frame(vec![1; 1000], true, 0).unwrap();
        sender.send_packet(vec![0xA2; 60]).unwrap();
        reel.send_frame(vec![2; 200], false, FRAME).unwrap();
        let mut buffer = sender.empty_buffer();
        while buffer.capacity() < 50 {
            thread::yield_now();
            buffer = sender.empty_buffer();
        }
        assert!(buffer.is_empty() && buffer.capacity() < 1000, "what held a packet comes back to hold a packet");
        let pending = reel.close();
        sender.send_packet(vec![0xA3; 70]).unwrap();
        drop(sender);
        let summary = pending.finish().unwrap().unwrap();
        let file = fs::read(&path).unwrap();
        assert_eq!((summary.samples, summary.packets, summary.bytes), (2, 3, file.len() as u64));
        assert_eq!(
            (file[HEAD], file[HEAD + 50], file[HEAD + 1050], file[HEAD + 1110], file[HEAD + 1310], file[HEAD + 1379]),
            (0xA1, 1, 0xA2, 2, 0xA3, 0xA3),
            "the packet that came before the first frame lies before it, and the file ends with the sound that ended last"
        );
        assert_eq!(&file[HEAD + 1384..HEAD + 1388], b"moov");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sound_starts_at_first_packet() {
        let (path, open) = scratch("note", "note.m4a");
        let (reel, sender) = Reel::start(open, Content::Sound(sound(), 1_791_500_000)).unwrap();
        for index in 0..5u8 {
            sender.send_packet(vec![index; 40]).unwrap();
        }
        drop(sender);
        let summary = reel.close().finish().unwrap().unwrap();
        let file = fs::read(&path).unwrap();
        assert_eq!((summary.samples, summary.packets, summary.ticks), (0, 5, 0));
        assert_eq!((&file[8..12], file[44], file[44 + 199]), (&b"M4A "[..], 0, 4));
        fs::remove_file(path).unwrap();
        let (path, open) = scratch("unused", "note.m4a");
        let (reel, sender) = Reel::start(open, Content::Sound(sound(), 0)).unwrap();
        drop(sender);
        assert_eq!(reel.close().finish().unwrap(), None);
        assert!(!path.exists(), "no packet, no file");
    }

    #[test]
    fn failed_sample_ends_file() {
        let (sender, receiver) = sync_channel(4);
        for index in 0..3u8 {
            sender.send(frame(vec![index + 1; 100], index == 0, u64::from(index) * FRAME)).unwrap();
        }
        let mut tape = Tape { bytes: Cursor::new(Vec::new()), writes: 0, failing_write: 4 };
        let mut spare = Vec::new();
        let content = Content::Video(track(), sound());
        let summary =
            write(&mut tape, String::new(), content, receiver.into_iter(), |bytes, _| spare.push(bytes)).unwrap();
        assert_eq!(spare.len(), 2, "what held the two samples that were written is handed back");
        assert!(
            sender.send(frame(Vec::new(), false, 0)).is_err(),
            "whoever sends samples learns that no more are taken"
        );
        let file = tape.bytes.into_inner();
        assert_eq!(
            (summary.samples, summary.packets, summary.bytes, summary.ticks),
            (2, 0, file.len() as u64, 2 * FRAME)
        );
        assert_eq!(word(&file, 44), 16 + 200);
        assert_eq!(
            (file[HEAD + 199], &file[HEAD + 204..HEAD + 208]),
            (2, &b"moov"[..]),
            "the index stands where the third sample began"
        );
        assert_eq!(word(&file, HEAD + 200), file.len() - HEAD - 200);
    }

    #[test]
    fn unusable_file_fails() {
        fn fails(open: impl FnOnce() -> io::Result<(File, String)> + Send + 'static) -> bool {
            let (reel, _) = Reel::start(open, Content::Video(track(), sound())).unwrap();
            reel.send_frame(vec![1; 10], true, 0).unwrap();
            reel.close().finish().is_err()
        }
        assert!(fails(|| Err(io::Error::other("no room for a name"))));
        assert!(fails(|| Ok((OpenOptions::new().write(true).open("/dev/full")?, "full".into()))));
    }
}
