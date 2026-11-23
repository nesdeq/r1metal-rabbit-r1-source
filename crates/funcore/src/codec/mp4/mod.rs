//! MP4 files of H.264 video, AAC sound or both: written with the index at the end, read by that index; sound is read as phones write it too.

mod read;
#[cfg(test)]
mod tests;
mod write;

pub(crate) use read::find_index;
pub use read::{IndexSeek, Movie, NO_SOUND_TRACK, Sample, Soundtrack, seek_index, units};
pub use write::{Content, Tail, VideoTrack, Writer};

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Pieces a file's writer may fall behind: 2 s of frames at 60 a second, or 2.5 s of sound.
pub const BACKLOG: usize = 120;
/// The error text of a file without its samples' box.
pub const NO_SAMPLES_BOX: &str = "a file without its samples' box";
const INDEX_BOX: &[u8; 4] = b"moov";
const BOX_HEADER: usize = 8;
const LARGE_SIZE: u32 = 1;
const STREAM_DESCRIPTOR: u8 = 3;
const DECODER_DESCRIPTOR: u8 = 4;
const DECODER_SPECIFICS: u8 = 5;
const MPEG4_AUDIO: u8 = 0x40;
const STREAM_FIELDS: usize = 2;
const SIZE_BYTES: usize = 4;
const SIZE_BITS: u32 = 7;
const SIZE_GOES_ON: u8 = 0x80;

/// The sample of that number in a file of `length` bytes, or the error text.
pub fn to_sample(samples: &[Sample], number: usize, length: u64) -> Result<&Sample, String> {
    let sample = samples.get(number).ok_or_else(|| format!("no sample {number}"))?;
    if sample.fits(length) { Ok(sample) } else { Err(format!("sample {number} reaches past the end of its file")) }
}

/// The description of a file's sound.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sound {
    /// Samples a second.
    pub rate: u32,
    /// Samples a packet.
    pub frame: u32,
    pub channels: u16,
    /// The decoder's configuration bytes.
    pub configuration: Vec<u8>,
    /// Samples a decoder gives before the file's first.
    pub lead: u32,
}
