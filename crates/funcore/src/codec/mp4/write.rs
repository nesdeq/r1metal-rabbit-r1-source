use super::{
    BOX_HEADER, DECODER_DESCRIPTOR, DECODER_SPECIFICS, INDEX_BOX, LARGE_SIZE, MPEG4_AUDIO, SIZE_BITS, SIZE_BYTES,
    SIZE_GOES_ON, STREAM_DESCRIPTOR, STREAM_FIELDS, Sound,
};
use crate::codec::h264;
use crate::codec::h264::write::LENGTH_BYTES;
use alloc::vec::Vec;

const EPOCH_GAP: u64 = 2_082_844_800;
const BRANDS: [&[u8; 4]; 4] = [b"isom", b"iso2", b"avc1", b"mp41"];
const SOUND_BRANDS: [&[u8; 4]; 3] = [b"M4A ", b"mp42", b"isom"];
const BRAND_VERSION: u32 = 0x200;
const SOUND_HANDLER_NAME: &[u8] = b"SoundHandler\0";
const CHUNK_PACKETS: u32 = 32;
const SAMPLE_BITS: u16 = 16;
pub(super) const LAYER_DESCRIPTOR: u8 = 6;
pub(super) const AUDIO_STREAM: u8 = 0x15;
pub(super) const NO_LAYER: u8 = 2;
const DECODER_BYTES: u32 = 768;
const VIDEO_TRACK_ID: u32 = 1;
pub(super) const UNIT_16_16: u32 = 1 << 16;
const UNIT_8_8: u16 = 1 << 8;
const UNIT_2_30: u32 = 1 << 30;
const LONG_TIMES: u32 = 1 << 24;
pub(super) const TRACK_FLAGS: u32 = 3;
const LANGUAGE_UNDETERMINED: u16 = 0x55C4;
const VIDEO_HANDLER_NAME: &[u8] = b"VideoHandler\0";
const SELF_CONTAINED: u32 = 1;
const VIDEO_HEADER_FLAGS: u32 = 1;
const RESOLUTION: u32 = 72 << 16;
const FRAMES_PER_SAMPLE: u16 = 1;
const COMPRESSOR_NAME_BYTES: usize = 32;
const DEPTH: u16 = 24;
const NO_COLOUR_TABLE: u16 = 0xFFFF;
const CONFIGURATION_VERSION: u8 = 1;
const LENGTH_FIELD: u8 = 0xFC | (LENGTH_BYTES as u8 - 1);
const ONE_SEQUENCE_SET: u8 = 0xE0 | 1;
const ONE_PICTURE_SET: u8 = 1;
const CHROMA_420: u8 = 0xFC | h264::CHROMA_420 as u8;
const EIGHT_BITS: u8 = 0xF8;
const COLOUR_KIND: &[u8; 4] = b"nclx";
const REC_709: u16 = h264::REC_709 as u16;
const VIDEO_RANGE: u8 = 0;

/// The description of a video track.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VideoTrack {
    pub width: u32,
    pub height: u32,
    /// Ticks of the track's clock a second.
    pub ticks_per_second: u32,
    /// Ticks a frame where none is lost.
    pub ticks_per_frame: u32,
    /// The sequence parameter set without its start code.
    pub sequence_parameters: Vec<u8>,
    /// The picture parameter set without its start code.
    pub picture_parameters: Vec<u8>,
    /// Clockwise quarter turns a player gives the pictures.
    pub quarters: u8,
    /// Seconds since the start of 1970.
    pub creation_time: u64,
}

/// What a recording holds.
pub enum Content {
    Video(VideoTrack, Sound),
    // The second is the creation time, in seconds since the start of 1970.
    Sound(Sound, u64),
}

impl Content {
    /// The index of a new file and its first bytes.
    pub fn begin(self) -> (Writer, Vec<u8>) {
        match self {
            Content::Video(track, sound) => Writer::new(track.creation_time, Some(track), sound),
            Content::Sound(sound, creation_time) => Writer::new(creation_time, None, sound),
        }
    }

    /// True where a piece begins a file: a frame, or for sound alone a packet.
    pub const fn starts_file(&self, is_frame: bool) -> bool {
        is_frame || matches!(self, Content::Sound(..))
    }
}

#[derive(Default)]
struct SampleTable {
    sizes: Vec<u32>,
    keys: Vec<u32>,
    durations: Vec<(u32, u32)>,
    chunks: Vec<(u64, u32)>,
    last_time: u64,
}

impl SampleTable {
    fn add_duration(&mut self, ticks: u32) {
        match self.durations.last_mut() {
            Some((count, duration)) if *duration == ticks => *count += 1,
            _ => self.durations.push((1, ticks)),
        }
    }

    fn add_sample(&mut self, place: u64, bytes: u32, is_chunk_start: bool) {
        match self.chunks.last_mut() {
            Some((_, count)) if !is_chunk_start => *count += 1,
            _ => self.chunks.push((place, 1)),
        }
        self.sizes.push(bytes);
    }
}

/// The index of a file while its samples are written.
pub struct Writer {
    track: Option<VideoTrack>,
    pictures: SampleTable,
    sound: Sound,
    packets: SampleTable,
    creation_time: u64,
    is_after_sound: bool,
    length: u64,
    size_at: u64,
}

/// The end of a file.
#[derive(PartialEq, Eq, Debug)]
pub struct Tail {
    /// The index, which goes behind the last sample.
    pub index: Vec<u8>,
    /// The samples' box size, for the eight bytes at `size_at`.
    pub size: [u8; 8],
    pub size_at: u64,
}

impl Tail {
    /// Ends a file held whole; false, and nothing changed, for bytes too few to hold the head.
    pub fn append_to(self, file: &mut Vec<u8>) -> bool {
        let at = self.size_at as usize;
        let Some(size) = file.get_mut(at..at + self.size.len()) else {
            return false;
        };
        size.copy_from_slice(&self.size);
        file.extend_from_slice(&self.index);
        true
    }
}

pub(super) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn put_u32s(out: &mut Vec<u8>, values: &[u32]) {
    values.iter().for_each(|&value| put_u32(out, value));
}

pub(super) fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn put_zeros(out: &mut Vec<u8>, count: usize) {
    out.resize(out.len() + count, 0);
}

pub(super) fn put_box(out: &mut Vec<u8>, name: &[u8; 4], fill: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    put_u32(out, 0);
    out.extend_from_slice(name);
    fill(out);
    let size = (out.len() - start) as u32;
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

pub(super) fn put_full_box(out: &mut Vec<u8>, name: &[u8; 4], version_and_flags: u32, fill: impl FnOnce(&mut Vec<u8>)) {
    put_box(out, name, |out| {
        put_u32(out, version_and_flags);
        fill(out);
    });
}

pub(super) fn put_table<T>(out: &mut Vec<u8>, name: &[u8; 4], entries: &[T], mut entry: impl FnMut(&mut Vec<u8>, &T)) {
    put_full_box(out, name, 0, |out| {
        put_u32(out, entries.len() as u32);
        entries.iter().for_each(|item| entry(out, item));
    });
}

pub(super) fn turn_matrix(quarters: u8, width: u32, height: u32) -> [u32; 9] {
    let minus_one = UNIT_16_16.wrapping_neg();
    let (a, b, c, d, x, y) = match quarters % 4 {
        0 => (UNIT_16_16, 0, 0, UNIT_16_16, 0, 0),
        1 => (0, UNIT_16_16, minus_one, 0, height, 0),
        2 => (minus_one, 0, 0, minus_one, width, height),
        _ => (0, minus_one, UNIT_16_16, 0, 0, width),
    };
    [a, b, 0, c, d, 0, x << 16, y << 16, UNIT_2_30]
}

impl Writer {
    fn new(creation_time: u64, track: Option<VideoTrack>, sound: Sound) -> (Writer, Vec<u8>) {
        let mut head = Vec::new();
        let brands = if track.is_some() { &BRANDS[..] } else { &SOUND_BRANDS[..] };
        put_box(&mut head, b"ftyp", |out| {
            out.extend_from_slice(brands[0]);
            put_u32(out, BRAND_VERSION);
            brands.iter().for_each(|brand| out.extend_from_slice(*brand));
        });
        let size_at = (head.len() + BOX_HEADER) as u64;
        put_u32(&mut head, LARGE_SIZE);
        head.extend_from_slice(b"mdat");
        put_u64(&mut head, 0);
        let length = head.len() as u64;
        let (pictures, packets) = (SampleTable::default(), SampleTable::default());
        (Writer { track, pictures, sound, packets, creation_time, is_after_sound: false, length, size_at }, head)
    }

    pub fn sample_count(&self) -> usize {
        self.pictures.sizes.len()
    }

    pub fn packet_count(&self) -> usize {
        self.packets.sizes.len()
    }

    /// Bytes of the file without its index.
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Ticks from the first sample of video to the end of the last.
    pub fn ticks(&self) -> u64 {
        match &self.track {
            Some(track) if !self.pictures.sizes.is_empty() => {
                self.pictures.last_time + u64::from(track.ticks_per_frame)
            }
            _ => 0,
        }
    }

    /// Notes a video sample the caller writes behind the file's last byte, at `time` ticks.
    pub fn add_sample(&mut self, bytes: u32, is_key: bool, time: u64) {
        let table = &mut self.pictures;
        let number = table.sizes.len() as u32 + 1;
        if number > 1 {
            let duration = time.saturating_sub(table.last_time).clamp(1, u64::from(u32::MAX)) as u32;
            table.add_duration(duration);
            table.last_time += u64::from(duration);
        }
        table.add_sample(self.length, bytes, is_key || self.is_after_sound);
        if is_key {
            table.keys.push(number);
        }
        self.is_after_sound = false;
        self.length += u64::from(bytes);
    }

    /// Notes a sound packet the caller writes behind the file's last byte.
    pub fn add_packet(&mut self, bytes: u32) {
        let is_full = self.packets.chunks.last().is_some_and(|&(_, count)| count >= CHUNK_PACKETS);
        self.packets.add_sample(self.length, bytes, is_full || !self.is_after_sound);
        self.is_after_sound = true;
        self.length += u64::from(bytes);
    }

    /// The index of all samples and the size of their box.
    pub fn finish(mut self) -> Tail {
        if let Some(track) = self.track.as_ref().filter(|_| !self.pictures.sizes.is_empty()) {
            self.pictures.add_duration(track.ticks_per_frame);
        }
        let sound = &self.sound;
        self.packets.durations = alloc::vec![(self.packets.sizes.len() as u32, sound.frame)];
        let video_ticks = self.ticks();
        let scale = self.track.as_ref().map_or(sound.rate, |track| track.ticks_per_second);
        let samples = (self.packets.sizes.len() as u64 * u64::from(sound.frame)).saturating_sub(u64::from(sound.lead));
        let sound_ticks = samples * u64::from(scale) / u64::from(sound.rate.max(1));
        let tracks = u32::from(self.track.is_some()) + 1;
        let mut index = Vec::new();
        put_box(&mut index, INDEX_BOX, |out| {
            self.put_movie_header(out, scale, video_ticks.max(sound_ticks), tracks);
            if let Some(track) = &self.track {
                put_box(out, b"trak", |out| {
                    self.put_track_header(out, VIDEO_TRACK_ID, video_ticks, Some(track));
                    put_box(out, b"mdia", |out| self.put_video_media(out, track, video_ticks));
                });
            }
            put_box(out, b"trak", |out| {
                self.put_track_header(out, tracks, sound_ticks, None);
                // The edit's times of 64 bits: in the video's ticks the sound passes 32 bits after two and a half hours.
                put_box(out, b"edts", |out| {
                    put_full_box(out, b"elst", LONG_TIMES, |out| {
                        put_u32(out, 1);
                        put_u64(out, sound_ticks);
                        put_u64(out, u64::from(sound.lead));
                        put_u32(out, UNIT_16_16);
                    });
                });
                put_box(out, b"mdia", |out| self.put_sound_media(out, sound));
            });
        });
        let size = self.length - (self.size_at - BOX_HEADER as u64);
        Tail { index, size: size.to_be_bytes(), size_at: self.size_at }
    }

    fn put_times(&self, out: &mut Vec<u8>) {
        put_u64(out, self.creation_time + EPOCH_GAP);
        put_u64(out, self.creation_time + EPOCH_GAP);
    }

    fn put_movie_header(&self, out: &mut Vec<u8>, scale: u32, ticks: u64, tracks: u32) {
        put_full_box(out, b"mvhd", LONG_TIMES, |out| {
            self.put_times(out);
            put_u32(out, scale);
            put_u64(out, ticks);
            put_u32(out, UNIT_16_16);
            put_u16(out, UNIT_8_8);
            put_zeros(out, 10);
            put_u32s(out, &turn_matrix(0, 0, 0));
            put_zeros(out, 24);
            put_u32(out, tracks + 1);
        });
    }

    fn put_track_header(&self, out: &mut Vec<u8>, number: u32, ticks: u64, video: Option<&VideoTrack>) {
        let (width, height, quarters) = video.map_or((0, 0, 0), |track| (track.width, track.height, track.quarters));
        put_full_box(out, b"tkhd", LONG_TIMES | TRACK_FLAGS, |out| {
            self.put_times(out);
            put_u32(out, number);
            put_zeros(out, 4);
            put_u64(out, ticks);
            put_zeros(out, 12);
            put_u16(out, if video.is_some() { 0 } else { UNIT_8_8 });
            put_zeros(out, 2);
            put_u32s(out, &turn_matrix(quarters, width, height));
            put_u32s(out, &[width << 16, height << 16]);
        });
    }

    fn put_media_header(&self, out: &mut Vec<u8>, scale: u32, ticks: u64, kind: &[u8; 4], name: &[u8]) {
        put_full_box(out, b"mdhd", LONG_TIMES, |out| {
            self.put_times(out);
            put_u32(out, scale);
            put_u64(out, ticks);
            put_u16(out, LANGUAGE_UNDETERMINED);
            put_zeros(out, 2);
        });
        put_full_box(out, b"hdlr", 0, |out| {
            put_zeros(out, 4);
            out.extend_from_slice(kind);
            put_zeros(out, 12);
            out.extend_from_slice(name);
        });
    }

    fn put_video_media(&self, out: &mut Vec<u8>, track: &VideoTrack, ticks: u64) {
        self.put_media_header(out, track.ticks_per_second, ticks, b"vide", VIDEO_HANDLER_NAME);
        put_box(out, b"minf", |out| {
            put_full_box(out, b"vmhd", VIDEO_HEADER_FLAGS, |out| put_zeros(out, 8));
            put_data_reference(out);
            put_box(out, b"stbl", |out| {
                put_full_box(out, b"stsd", 0, |out| {
                    put_u32(out, 1);
                    put_video_entry(track, out);
                });
                put_sample_tables(out, &self.pictures, true);
            });
        });
    }

    fn put_sound_media(&self, out: &mut Vec<u8>, sound: &Sound) {
        let packets = &self.packets.sizes;
        let samples = packets.len() as u64 * u64::from(sound.frame);
        self.put_media_header(out, sound.rate, samples, b"soun", SOUND_HANDLER_NAME);
        let bit_rate =
            |bytes: u64, samples: u64| (bytes * u64::from(u8::BITS) * u64::from(sound.rate) / samples.max(1)) as u32;
        let max_rate = bit_rate(u64::from(packets.iter().copied().max().unwrap_or(0)), u64::from(sound.frame));
        let mean_rate = bit_rate(packets.iter().map(|&bytes| u64::from(bytes)).sum(), samples);
        put_box(out, b"minf", |out| {
            put_full_box(out, b"smhd", 0, |out| put_zeros(out, 4));
            put_data_reference(out);
            put_box(out, b"stbl", |out| {
                put_full_box(out, b"stsd", 0, |out| {
                    put_u32(out, 1);
                    put_box(out, b"mp4a", |out| {
                        put_zeros(out, 6);
                        put_u16(out, 1);
                        put_zeros(out, 8);
                        put_u16(out, sound.channels);
                        put_u16(out, SAMPLE_BITS);
                        put_zeros(out, 4);
                        put_u32(out, sound.rate << 16);
                        put_full_box(out, b"esds", 0, |out| {
                            put_descriptor(out, STREAM_DESCRIPTOR, |out| {
                                put_zeros(out, STREAM_FIELDS + 1);
                                put_descriptor(out, DECODER_DESCRIPTOR, |out| {
                                    out.extend_from_slice(&[MPEG4_AUDIO, AUDIO_STREAM]);
                                    out.extend_from_slice(&DECODER_BYTES.to_be_bytes()[1..]);
                                    put_u32s(out, &[max_rate, mean_rate]);
                                    put_descriptor(out, DECODER_SPECIFICS, |out| {
                                        out.extend_from_slice(&sound.configuration)
                                    });
                                });
                                put_descriptor(out, LAYER_DESCRIPTOR, |out| out.push(NO_LAYER));
                            });
                        });
                    });
                });
                put_sample_tables(out, &self.packets, false);
            });
        });
    }
}

fn put_data_reference(out: &mut Vec<u8>) {
    put_box(out, b"dinf", |out| {
        put_full_box(out, b"dref", 0, |out| {
            put_u32(out, 1);
            put_full_box(out, b"url ", SELF_CONTAINED, |_| {});
        });
    });
}

fn put_descriptor(out: &mut Vec<u8>, tag: u8, fill: impl FnOnce(&mut Vec<u8>)) {
    out.push(tag);
    let start = out.len();
    fill(out);
    let size = out.len() - start;
    let bytes = 1 + (1..SIZE_BYTES).take_while(|&byte| size >> (SIZE_BITS as usize * byte) != 0).count();
    let size_bytes = (0..bytes).rev().map(|byte| {
        let bits = (size >> (SIZE_BITS as usize * byte)) as u8 & !SIZE_GOES_ON;
        if byte == 0 { bits } else { bits | SIZE_GOES_ON }
    });
    out.splice(start..start, size_bytes);
}

fn put_sample_tables(out: &mut Vec<u8>, track: &SampleTable, has_keys: bool) {
    put_table(out, b"stts", &track.durations, |out, &(count, ticks)| put_u32s(out, &[count, ticks]));
    if has_keys {
        put_table(out, b"stss", &track.keys, |out, &number| put_u32(out, number));
    }
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (index, &(_, samples)) in track.chunks.iter().enumerate() {
        if runs.last().is_none_or(|&(_, run_samples)| run_samples != samples) {
            runs.push((index as u32 + 1, samples));
        }
    }
    put_table(out, b"stsc", &runs, |out, &(first, samples)| put_u32s(out, &[first, samples, 1]));
    put_full_box(out, b"stsz", 0, |out| {
        put_u32s(out, &[0, track.sizes.len() as u32]);
        put_u32s(out, &track.sizes);
    });
    put_table(out, b"co64", &track.chunks, |out, &(place, _)| put_u64(out, place));
}

fn put_video_entry(track: &VideoTrack, out: &mut Vec<u8>) {
    put_box(out, b"avc1", |out| {
        put_zeros(out, 6);
        put_u16(out, 1);
        put_zeros(out, 16);
        put_u16(out, track.width as u16);
        put_u16(out, track.height as u16);
        put_u32s(out, &[RESOLUTION; 2]);
        put_zeros(out, 4);
        put_u16(out, FRAMES_PER_SAMPLE);
        put_zeros(out, COMPRESSOR_NAME_BYTES);
        put_u16(out, DEPTH);
        put_u16(out, NO_COLOUR_TABLE);
        put_box(out, b"avcC", |out| {
            let sequence = &track.sequence_parameters;
            out.push(CONFIGURATION_VERSION);
            out.extend_from_slice(sequence.get(1..4).unwrap_or(&[0; 3]));
            out.extend_from_slice(&[LENGTH_FIELD, ONE_SEQUENCE_SET]);
            put_u16(out, sequence.len() as u16);
            out.extend_from_slice(sequence);
            out.push(ONE_PICTURE_SET);
            put_u16(out, track.picture_parameters.len() as u16);
            out.extend_from_slice(&track.picture_parameters);
            out.extend_from_slice(&[CHROMA_420, EIGHT_BITS, EIGHT_BITS, 0]);
        });
        put_box(out, b"colr", |out| {
            out.extend_from_slice(COLOUR_KIND);
            (0..3).for_each(|_| put_u16(out, REC_709));
            out.push(VIDEO_RANGE);
        });
    });
}
