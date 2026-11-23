use crate::sys::invalid;
use r1_funcore::codec::Flaw;
use r1_funcore::codec::aac;
use r1_funcore::codec::h264::read::Sequence;
use r1_funcore::codec::mp4::{self, Content, IndexSeek, Movie, NO_SAMPLES_BOX, Sample, Soundtrack};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

fn read_part(file: &File, offset: u64, length: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
    bytes.resize(length, 0);
    file.read_exact_at(bytes, offset)
}

fn read_index<T>(file: &File, parse: fn(&[u8], u64) -> Result<T, Flaw>) -> io::Result<(u64, T)> {
    let length = file.metadata()?.len();
    let (mut bytes, mut seek) = (Vec::new(), IndexSeek::start(length));
    loop {
        match seek {
            IndexSeek::Next { at, bytes: count } => {
                read_part(file, at, count, &mut bytes)?;
                seek = mp4::seek_index(&bytes, at, length).map_err(|flaw| invalid(flaw.to_string()))?;
            }
            IndexSeek::Index { at, bytes: count } => {
                read_part(file, at, count, &mut bytes)?;
                return Ok((length, parse(&bytes, length).map_err(|flaw| invalid(flaw.to_string()))?));
            }
        }
    }
}

fn read_sample(file: &File, length: u64, samples: &[Sample], number: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
    let sample = mp4::to_sample(samples, number, length).map_err(invalid)?;
    read_part(file, sample.at, sample.bytes as usize, bytes)
}

/// A video file with its index read.
pub struct VideoFile {
    file: File,
    length: u64,
    pub(crate) movie: Movie,
}

impl VideoFile {
    /// Reads the index of a file for its H.264 track.
    pub fn open(file: File) -> io::Result<VideoFile> {
        let (length, movie) = read_index(&file, Movie::read)?;
        Ok(VideoFile { file, length, movie })
    }

    /// The stream's sequence parameters, from its index (`Movie::sequence`).
    pub fn sequence(&self) -> io::Result<Sequence> {
        self.movie.sequence().map_err(|flaw| invalid(flaw.to_string()))
    }

    /// Reads sample `number`, from 0 in decoding order, into `bytes`.
    pub fn sample(&self, number: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
        read_sample(&self.file, self.length, &self.movie.samples, number, bytes)
    }
}

/// The sound of a sound recording or a video, with its index read.
pub struct SoundFile {
    file: File,
    pub(crate) length: u64,
    pub(crate) track: Soundtrack,
}

impl SoundFile {
    /// Reads the index of a file for its AAC track; none where the file has no sound track.
    pub fn open(file: File) -> io::Result<Option<SoundFile>> {
        let (length, track) = read_index(&file, aac::read_index)?;
        Ok(track.map(|track| SoundFile { file, length, track }))
    }

    pub fn read_all(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        read_part(&self.file, 0, self.length as usize, &mut bytes)?;
        Ok(bytes)
    }

    /// Reads packet `number`, from 0, into `bytes`.
    pub fn packet(&self, number: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
        read_sample(&self.file, self.length, &self.track.packets, number, bytes)
    }

    /// A sound file of `count` packets from `first`, created at `created` in Unix seconds, for a recording too long to send whole.
    pub fn cut(&self, first: usize, count: usize, created: u64) -> io::Result<Vec<u8>> {
        let sound = self.track.cut_from(first);
        let ((mut writer, mut file), mut packet) = (Content::Sound(sound, created).begin(), Vec::new());
        for number in first..first + count {
            self.packet(number, &mut packet)?;
            writer.add_packet(packet.len() as u32);
            file.extend_from_slice(&packet);
        }
        writer.finish().append_to(&mut file).then_some(file).ok_or_else(|| invalid(NO_SAMPLES_BOX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::codec::aac::{FRAME, cut_seconds, track_seconds};
    use r1_funcore::codec::h264::write::Stream;
    use r1_funcore::codec::mp4::{Sound, VideoTrack};
    use std::fs;
    use std::path::PathBuf;

    const STREAM: Stream =
        Stream { width: 1280, height: 720, level: 41, pace: Some((7999, 480_000)), is_chroma_on_luma: true };

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("r1-clip-{name}-{}.mp4", std::process::id()))
    }

    fn recording(samples: &[&[u8]]) -> Vec<u8> {
        let track = VideoTrack {
            width: 1280,
            height: 720,
            ticks_per_second: 480_000,
            ticks_per_frame: 7999,
            sequence_parameters: STREAM.sequence_parameters(),
            picture_parameters: STREAM.picture_parameters(),
            quarters: 1,
            creation_time: 1_791_500_000,
        };
        let sound =
            Sound { rate: 48_000, frame: FRAME as u32, channels: 1, configuration: vec![0x11, 0x88], lead: 1024 };
        let (mut writer, mut file) = Content::Video(track, sound).begin();
        for (number, sample) in samples.iter().enumerate() {
            writer.add_sample(sample.len() as u32, number == 0, number as u64 * 7999);
            file.extend_from_slice(sample);
        }
        assert!(writer.finish().append_to(&mut file));
        file
    }

    #[test]
    fn track_and_samples() {
        let path = scratch("whole");
        fs::write(&path, recording(&[&[1; 300], &[2; 40], &[3; 70]])).unwrap();
        let video = VideoFile::open(File::open(&path).unwrap()).unwrap();
        let movie = &video.movie;
        assert_eq!(
            (movie.width, movie.height, movie.quarters, movie.samples.len(), movie.frame_ticks()),
            (1280, 720, 1, 3, 7999)
        );
        assert_eq!(video.sequence().unwrap().size(), (1280, 720));
        let mut bytes = vec![9; 5];
        video.sample(1, &mut bytes).unwrap();
        assert_eq!(bytes, [2; 40]);
        video.sample(0, &mut bytes).unwrap();
        assert_eq!(bytes, [1; 300], "the buffer takes the size of each sample");
        assert!(video.sample(3, &mut bytes).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn refuse_bad_index() {
        let file = recording(&[&[1; 2000], &[2; 40]]);
        let index_at = file.windows(4).position(|name| name == b"moov").unwrap() - 4;
        let path = scratch("cut");
        fs::write(&path, &file[..index_at]).unwrap();
        assert_eq!(
            VideoFile::open(File::open(&path).unwrap()).err().map(|e| e.kind()),
            Some(io::ErrorKind::InvalidData),
            "a recording that was never ended"
        );
        fs::write(&path, &file[..file.len() - 9]).unwrap();
        assert!(VideoFile::open(File::open(&path).unwrap()).is_err(), "an index that is cut off");
        fs::write(&path, b"no boxes here at all").unwrap();
        assert!(VideoFile::open(File::open(&path).unwrap()).is_err());
        fs::write(&path, &file[index_at..]).unwrap();
        let video = VideoFile::open(File::open(&path).unwrap()).unwrap();
        assert!(
            file.len() - index_at < 2000 && video.sample(0, &mut Vec::new()).is_err(),
            "the index names bytes the file does not have"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn cut_keeps_lead_from_first() {
        let sound =
            Sound { rate: 48_000, frame: FRAME as u32, channels: 1, configuration: vec![0x11, 0x88], lead: 1024 };
        let (mut writer, mut file) = Content::Sound(sound, 1_791_500_000).begin();
        for number in 0..100u8 {
            let packet = vec![number; 10 + usize::from(number)];
            writer.add_packet(packet.len() as u32);
            file.extend_from_slice(&packet);
        }
        assert!(writer.finish().append_to(&mut file));
        let (path, cut) = (scratch("sound"), scratch("piece"));
        fs::write(&path, &file).unwrap();
        let sound_file = SoundFile::open(File::open(&path).unwrap()).unwrap().unwrap();
        let read = |piece: Vec<u8>| {
            fs::write(&cut, piece).unwrap();
            SoundFile::open(File::open(&cut).unwrap()).unwrap().unwrap()
        };
        let next = read(sound_file.cut(40, 30, 1_791_500_000).unwrap());
        let mut bytes = Vec::new();
        next.packet(0, &mut bytes).unwrap();
        assert_eq!(
            (next.track.packets.len(), next.track.sound.lead, bytes, track_seconds(&next.track)),
            (30, 0, vec![40; 50], cut_seconds(&sound_file.track, 40, 30)),
            "a later cut holds its packets from the one asked for, without the lead"
        );
        let first = read(sound_file.cut(0, 10, 1_791_500_000).unwrap());
        assert_eq!(
            (first.track.packets.len(), first.track.sound.lead, first.track.sound.rate, track_seconds(&first.track)),
            (10, 1024, 48_000, cut_seconds(&sound_file.track, 0, 10)),
            "the first keeps the lead"
        );
        fs::remove_file(path).unwrap();
        fs::remove_file(cut).unwrap();
    }
}
