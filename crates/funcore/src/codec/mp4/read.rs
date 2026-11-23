use super::{
    BOX_HEADER, DECODER_DESCRIPTOR, DECODER_SPECIFICS, INDEX_BOX, LARGE_SIZE, MPEG4_AUDIO, SIZE_BITS, SIZE_BYTES,
    SIZE_GOES_ON, STREAM_DESCRIPTOR, STREAM_FIELDS, Sound,
};
use crate::base::bytes::be16;
use crate::base::bytes::be32;
use crate::codec::Flaw;
use crate::codec::h264::read::Sequence;
use alloc::vec::Vec;
use core::iter;

const DECODER_FIELDS: usize = 13;
pub(super) const SOUND_ENTRY_BYTES: usize = 28;
pub(super) const SOUND_ENTRY_CHANNELS_AT: usize = 16;
pub(super) const SOUND_ENTRY_VERSION_AT: usize = 8;
// What an entry of version 1 holds behind those: QuickTime's four fields of a compressed sound's packets.
pub(super) const SOUND_ENTRY_BYTES_OF_VERSION_1: usize = 16;
// What an entry of version 2 holds behind them: the size of the structure, the rate as a double, the channels, 0x7F000000, the bits of a channel, the format's flags, and the bytes and frames of a packet.
pub(super) const SOUND_ENTRY_BYTES_OF_VERSION_2: usize = 36;
pub(super) const SOUND_ENTRY_VERSION_2_CHANNELS_AT: usize = 12;
const WRAPPING_BOX: &[u8; 4] = b"wave";
// A stream descriptor's flags for the fields that may follow them: another stream it depends on, an address behind its length, a clock (ES_Descriptor, 14496-1).
pub(super) const DEPENDS_ON_STREAM: u8 = 0x80;
pub(super) const ADDRESS_FOLLOWS: u8 = 0x40;
pub(super) const CLOCK_STREAM: u8 = 0x20;
const STREAM_REFERENCE_BYTES: usize = 2;
const LARGE_BOX_HEADER: usize = 16;
const TO_THE_END: u32 = 0;
pub(super) const VIDEO_ENTRY_BYTES: usize = 78;
pub(super) const VIDEO_ENTRY_WIDTH_AT: usize = 24;
const MATRIX_AT: [usize; 2] = [40, 52];
const LENGTH_MASK: u8 = 3;
const SET_COUNT_MASK: u8 = 0x1F;

/// A video sample's or sound packet's place in its file and its time in ticks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sample {
    pub at: u64,
    pub bytes: u32,
    pub(crate) time: u64,
    pub(crate) is_key: bool,
}

impl Sample {
    pub(crate) fn fits(&self, length: u64) -> bool {
        self.at.checked_add(u64::from(self.bytes)).is_some_and(|end| end <= length)
    }
}

/// The video track of a file, from its index.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Movie {
    pub width: u32,
    pub height: u32,
    /// Clockwise quarter turns a player gives the pictures.
    pub quarters: u8,
    pub ticks_per_second: u32,
    pub(super) ticks: u64,
    /// Bytes of the length before each unit of a sample.
    pub length_bytes: usize,
    /// The sequence parameter sets the index holds.
    pub sequence_parameters: Vec<Vec<u8>>,
    /// The picture parameter sets the index holds.
    pub picture_parameters: Vec<Vec<u8>>,
    /// The samples in decoding order.
    pub samples: Vec<Sample>,
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Flaw> {
        let (head, rest) = self.0.split_at_checked(count).ok_or(Flaw::Short)?;
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, Flaw> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Flaw> {
        be16(self.take(2)?, 0).ok_or(Flaw::Short)
    }

    fn u32(&mut self) -> Result<u32, Flaw> {
        be32(self.take(4)?, 0).ok_or(Flaw::Short)
    }

    fn table(&mut self, words: usize) -> Result<impl Iterator<Item = &'a [u8]> + use<'a>, Flaw> {
        let count = self.u32()? as usize;
        Ok(self.0.chunks_exact(4 * words).take(count))
    }
}

fn u32_at(entry: &[u8], index: usize) -> u32 {
    be32(entry, 4 * index).unwrap_or(0)
}

pub(super) fn parse_box_start(start: &[u8]) -> Option<([u8; 4], u64, usize)> {
    let size = be32(start, 0)?;
    let name: [u8; 4] = start.get(4..BOX_HEADER)?.try_into().ok()?;
    if size != LARGE_SIZE {
        return Some((name, u64::from(size), BOX_HEADER));
    }
    Some((name, u64::from_be_bytes(start.get(BOX_HEADER..LARGE_BOX_HEADER)?.try_into().ok()?), LARGE_BOX_HEADER))
}

fn fit_size(size: u64, header: usize, room: u64) -> Option<u64> {
    let size = if size == u64::from(TO_THE_END) { room } else { size };
    (size >= header as u64 && size <= room).then_some(size)
}

/// The next read of a walk through a file's boxes for its index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexSeek {
    Next { at: u64, bytes: usize },
    Index { at: u64, bytes: usize },
}

impl IndexSeek {
    /// The opening of the first box of a file of `length` bytes.
    pub fn start(length: u64) -> IndexSeek {
        IndexSeek::box_at(0, length)
    }

    fn box_at(at: u64, length: u64) -> IndexSeek {
        IndexSeek::Next { at, bytes: length.saturating_sub(at).min(LARGE_BOX_HEADER as u64) as usize }
    }
}

/// One step of the walk, `start` being what was read of the opening at `at`.
pub fn seek_index(start: &[u8], at: u64, length: u64) -> Result<IndexSeek, Flaw> {
    if at >= length {
        return Err(Flaw::Missing("the index"));
    }
    let (name, size, header) = parse_box_start(start).ok_or(Flaw::Short)?;
    let size = fit_size(size, header, length - at).ok_or(Flaw::Short)?;
    if &name == INDEX_BOX {
        return Ok(IndexSeek::Index { at: at + header as u64, bytes: (size - header as u64) as usize });
    }
    Ok(IndexSeek::box_at(at + size, length))
}

pub(crate) fn find_index(file: &[u8]) -> Result<&[u8], Flaw> {
    let length = file.len() as u64;
    let mut seek = IndexSeek::start(length);
    loop {
        match seek {
            IndexSeek::Next { at, bytes } => seek = seek_index(&file[at as usize..][..bytes], at, length)?,
            IndexSeek::Index { at, bytes } => return Ok(&file[at as usize..][..bytes]),
        }
    }
}

pub(super) fn boxes(bytes: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = bytes;
    iter::from_fn(move || {
        let (name, size, header) = parse_box_start(rest)?;
        let size = usize::try_from(fit_size(size, header, rest.len() as u64)?).ok()?;
        let (content, tail) = rest.split_at(size);
        rest = tail;
        Some((name, &content[header..]))
    })
}

pub(super) fn find_box<'a>(bytes: &'a [u8], name: &'static [u8; 4]) -> Option<&'a [u8]> {
    boxes(bytes).find(|(box_name, _)| box_name == name).map(|(_, content)| content)
}

fn read_full_box<'a>(bytes: &'a [u8], name: &'static [u8; 4], what: &'static str) -> Result<(Cursor<'a>, u8), Flaw> {
    let mut fields = Cursor(find_box(bytes, name).ok_or(Flaw::Missing(what))?);
    let version = fields.byte()?;
    fields.take(3)?;
    Ok((fields, version))
}

struct Media<'a> {
    rate: u32,
    tables: &'a [u8],
    entry: ([u8; 4], &'a [u8]),
}

fn read_media<'a>(track: &'a [u8], handler: &[u8; 4]) -> Result<Option<Media<'a>>, Flaw> {
    let media = find_box(track, b"mdia").ok_or(Flaw::Missing("a track's media"))?;
    let (mut kind, _) = read_full_box(media, b"hdlr", "a track's kind")?;
    kind.take(4)?;
    if kind.take(4)? != handler {
        return Ok(None);
    }
    let (mut timing, version) = read_full_box(media, b"mdhd", "a track's clock")?;
    timing.take(if version == 1 { 16 } else { 8 })?;
    let rate = timing.u32()?;
    let tables =
        find_box(media, b"minf").and_then(|inner| find_box(inner, b"stbl")).ok_or(Flaw::Missing("a track's tables"))?;
    let (mut descriptions, _) = read_full_box(tables, b"stsd", "what the samples are")?;
    descriptions.u32()?;
    let entry = boxes(descriptions.0).next().ok_or(Flaw::Short)?;
    Ok(Some(Media { rate, tables, entry }))
}

// The first track `read_track` reads; none where the index holds no track of its kind, and the last flaw where every one of its kind has one.
fn first_track<T>(
    index: &[u8],
    length: u64,
    read_track: impl Fn(&[u8], u64) -> Result<Option<T>, Flaw>,
) -> Result<Option<T>, Flaw> {
    let mut result = Ok(None);
    for (_, track) in boxes(index).filter(|(name, _)| name == b"trak") {
        match read_track(track, length) {
            Ok(None) => {}
            Ok(Some(value)) => return Ok(Some(value)),
            Err(fault) => result = Err(fault),
        }
    }
    result
}

/// The units of a sample, each behind its length of `length_bytes` bytes.
pub fn units(sample: &[u8], length_bytes: usize) -> impl Iterator<Item = &[u8]> {
    let mut rest = sample;
    iter::from_fn(move || {
        let (length, tail) = rest.split_at_checked(length_bytes)?;
        let length = length.iter().fold(0usize, |length, &byte| length << 8 | usize::from(byte));
        let (unit, tail) = tail.split_at_checked(length)?;
        rest = tail;
        Some(unit)
    })
}

fn to_quarters(matrix: &[u8]) -> u8 {
    let sign = |index: usize| (u32_at(matrix, index) as i32).signum();
    match (sign(0), sign(1), sign(3), sign(4)) {
        (0, 1, -1, 0) => 1,
        (-1, 0, 0, -1) => 2,
        (0, -1, 1, 0) => 3,
        _ => 0,
    }
}

impl Movie {
    /// Reads the first H.264 track from the content of a `moov` box of a file of `length` bytes.
    pub fn read(index: &[u8], length: u64) -> Result<Movie, Flaw> {
        first_track(index, length, Movie::read_track)?.ok_or(Flaw::Missing("a video track"))
    }

    /// The first sequence parameter set the index holds.
    pub fn sequence(&self) -> Result<Sequence, Flaw> {
        Sequence::read(self.sequence_parameters.first().ok_or(Flaw::Missing("sequence parameters in the index"))?)
    }

    /// Ticks a picture where all last equally long.
    pub fn frame_ticks(&self) -> u64 {
        self.ticks.checked_div(self.samples.len() as u64).unwrap_or(0)
    }

    /// Whole seconds of the track.
    pub fn seconds(&self) -> u64 {
        self.ticks / u64::from(self.ticks_per_second.max(1))
    }

    /// The number of the first key sample.
    pub fn first_key(&self) -> Option<usize> {
        self.samples.iter().position(|sample| sample.is_key)
    }

    /// The time of that sample in ticks; 0 for one the track does not hold.
    pub fn time_of(&self, number: usize) -> u64 {
        self.samples.get(number).map_or(0, |sample| sample.time)
    }

    fn read_track(track: &[u8], length: u64) -> Result<Option<Movie>, Flaw> {
        let Some(Media { rate: ticks_per_second, tables, entry: (kind, entry) }) = read_media(track, b"vide")? else {
            return Ok(None);
        };
        let header = find_box(track, b"tkhd").ok_or(Flaw::Missing("a track's header"))?;
        let matrix = header.get(MATRIX_AT[usize::from(header.first() == Some(&1))]..).ok_or(Flaw::Short)?;
        if &kind != b"avc1" {
            return Err(Flaw::Unsupported("video that is not H.264 in avc1"));
        }
        let fields = entry.get(..VIDEO_ENTRY_BYTES).ok_or(Flaw::Short)?;
        let size = |at: usize| be16(fields, at).map_or(0, u32::from);
        let mut configuration =
            Cursor(find_box(&entry[VIDEO_ENTRY_BYTES..], b"avcC").ok_or(Flaw::Missing("the decoder configuration"))?);
        configuration.take(4)?;
        let length_bytes = usize::from(configuration.byte()? & LENGTH_MASK) + 1;
        let mut sets = |mask: u8| -> Result<Vec<Vec<u8>>, Flaw> {
            let count = configuration.byte()? & mask;
            (0..count)
                .map(|_| {
                    configuration.u16().and_then(|length| configuration.take(usize::from(length))).map(<[u8]>::to_vec)
                })
                .collect()
        };
        let (sequence_parameters, picture_parameters) = (sets(SET_COUNT_MASK)?, sets(u8::MAX)?);
        let (samples, ticks) = read_samples(tables, length, false)?;
        Ok(Some(Movie {
            width: size(VIDEO_ENTRY_WIDTH_AT),
            height: size(VIDEO_ENTRY_WIDTH_AT + 2),
            quarters: to_quarters(matrix),
            ticks_per_second,
            ticks,
            length_bytes,
            sequence_parameters,
            picture_parameters,
            samples,
        }))
    }
}

// Sound has no samples shown out of the order they are decoded in, and may place its chunks in 32 bits.
fn read_samples(tables: &[u8], length: u64, is_sound: bool) -> Result<(Vec<Sample>, u64), Flaw> {
    if !is_sound && find_box(tables, b"ctts").is_some() {
        return Err(Flaw::Unsupported("samples shown out of the order they are decoded in"));
    }
    let (mut sizes, _) = read_full_box(tables, b"stsz", "the sizes of the samples")?;
    let (fixed_size, count) = (sizes.u32()?, sizes.u32()? as usize);
    // The count is the file's word: a table of sizes bounds it, and a fixed size the file's bytes.
    let room = if fixed_size == 0 {
        sizes.0.len() / 4
    } else {
        usize::try_from(length / u64::from(fixed_size)).unwrap_or(usize::MAX)
    };
    let count = count.min(room);
    let sizes = sizes.0;
    let (mut runs, _) = read_full_box(tables, b"stsc", "the chunks of the samples")?;
    let runs: Vec<(u32, u32)> = runs.table(3)?.map(|entry| (u32_at(entry, 0), u32_at(entry, 1))).collect();
    let chunks: Vec<u64> = match read_full_box(tables, b"co64", "the places of the chunks") {
        Ok((mut places, _)) => {
            places.table(2)?.map(|entry| u64::from(u32_at(entry, 0)) << 32 | u64::from(u32_at(entry, 1))).collect()
        }
        Err(_) if is_sound => {
            let (mut places, _) = read_full_box(tables, b"stco", "the places of the chunks")?;
            places.table(1)?.map(|entry| u64::from(u32_at(entry, 0))).collect()
        }
        Err(missing) => return Err(missing),
    };
    let (mut durations, _) = read_full_box(tables, b"stts", "the times of the samples")?;
    let mut durations =
        durations.table(2)?.flat_map(|entry| iter::repeat_n(u64::from(u32_at(entry, 1)), u32_at(entry, 0) as usize));
    let keys: Option<Vec<u32>> = read_full_box(tables, b"stss", "")
        .ok()
        .and_then(|(mut keys, _)| keys.table(1).ok())
        .map(|keys| keys.map(|entry| u32_at(entry, 0)).collect());

    // The lists grow with the chunks, not by the count, which is the file's word.
    let (mut samples, mut clock, mut run) = (Vec::new(), 0u64, 0);
    'chunks: for (chunk, &place) in chunks.iter().enumerate() {
        while runs.get(run + 1).is_some_and(|next| next.0 as usize <= chunk + 1) {
            run += 1;
        }
        let mut at = place;
        for _ in 0..runs.get(run).map_or(0, |run| run.1) {
            let number = samples.len();
            if number == count {
                break 'chunks;
            }
            let bytes = if fixed_size == 0 { u32_at(sizes, number) } else { fixed_size };
            let is_key = keys.as_ref().is_none_or(|keys| keys.binary_search(&(number as u32 + 1)).is_ok());
            samples.push(Sample { at, bytes, time: clock, is_key });
            clock += durations.next().unwrap_or(0);
            at = at.saturating_add(u64::from(bytes));
        }
    }
    if samples.len() < count {
        return Err(Flaw::Missing("chunks for all samples"));
    }
    Ok((samples, clock))
}

/// The flaw of a file that is read for its sound and has no sound track.
pub const NO_SOUND_TRACK: Flaw = Flaw::Missing("a sound track");

/// The sound track of a file, from its index.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Soundtrack {
    pub sound: Sound,
    /// The packets in order, each with the sample it begins at as its time.
    pub packets: Vec<Sample>,
    // Samples the file's edit lets be heard, from the one the sound begins with; none where the edits set no end.
    pub(crate) heard: Option<u64>,
}

pub(super) fn read_descriptor(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, mut rest) = bytes.split_first()?;
    let mut size = 0usize;
    for _ in 0..SIZE_BYTES {
        let (&byte, tail) = rest.split_first()?;
        rest = tail;
        size = size << SIZE_BITS | usize::from(byte & !SIZE_GOES_ON);
        if byte & SIZE_GOES_ON == 0 {
            break;
        }
    }
    let (content, tail) = rest.split_at_checked(size)?;
    Some((tag, content, tail))
}

fn find_descriptor(bytes: &[u8], tag: u8) -> Option<&[u8]> {
    let mut rest = bytes;
    iter::from_fn(move || {
        let (descriptor_tag, content, tail) = read_descriptor(rest)?;
        rest = tail;
        Some((descriptor_tag, content))
    })
    .find(|&(descriptor_tag, _)| descriptor_tag == tag)
    .map(|(_, content)| content)
}

// Ticks of the movie's clock in a second, as the movie header tells; none where there is no header.
fn movie_ticks(index: &[u8]) -> Option<u32> {
    let (mut header, version) = read_full_box(index, b"mvhd", "the movie's clock").ok()?;
    header.take(if version == 1 { 16 } else { 8 }).ok()?;
    header.u32().ok()
}

// What the edit list of a sound track whose media's clock ticks `rate` times a second makes of it: the samples before the first edit that plays sound begins, and, where that edit is the only one and the movie's clock is known, the samples it lasts. Empty edits wait before the sound and are passed over.
fn read_edits(track: &[u8], rate: u32, movie: Option<u32>) -> Result<(u32, Option<u64>), Flaw> {
    let Some((mut list, version)) = find_box(track, b"edts").and_then(|edits| read_full_box(edits, b"elst", "").ok())
    else {
        return Ok((0, None));
    };
    if version > 1 {
        return Err(Flaw::Unsupported("an edit list of a version past 1"));
    }
    // The duration of an edit in the movie's ticks and where it begins in the media's, none for an empty edit: times of 64 bits in version 1, of 32 in version 0.
    let entry = |entry: &[u8]| {
        if version == 1 {
            let begin = u64::from(u32_at(entry, 2)) << 32 | u64::from(u32_at(entry, 3));
            (u64::from(u32_at(entry, 0)) << 32 | u64::from(u32_at(entry, 1)), (begin as i64 >= 0).then_some(begin))
        } else {
            let begin = u32_at(entry, 1);
            (u64::from(u32_at(entry, 0)), (begin as i32 >= 0).then_some(u64::from(begin)))
        }
    };
    let playing: Vec<(u64, u64)> = list
        .table(if version == 1 { 5 } else { 3 })?
        .filter_map(|each| {
            let (duration, begin) = entry(each);
            begin.map(|begin| (duration, begin))
        })
        .collect();
    let Some(&(duration, begin)) = playing.first() else {
        return Ok((0, None));
    };
    let lead = u32::try_from(begin).unwrap_or(0);
    let heard = match (playing.len(), movie) {
        (1, Some(ticks)) => {
            u64::try_from((u128::from(duration) * u128::from(rate)).div_ceil(u128::from(ticks.max(1)))).ok()
        }
        _ => None,
    };
    Ok((lead, heard))
}

// The channels and the bytes an entry holds before the boxes behind its fields, by its version: QuickTime's version 1 has four fields more, and version 2 the channels in a field of 32 bits.
fn read_sound_entry(entry: &[u8]) -> Result<(u16, usize), Flaw> {
    let fields = entry.get(..SOUND_ENTRY_BYTES).ok_or(Flaw::Short)?;
    let version = be16(fields, SOUND_ENTRY_VERSION_AT).ok_or(Flaw::Short)?;
    let channels = be16(fields, SOUND_ENTRY_CHANNELS_AT).ok_or(Flaw::Short)?;
    match version {
        0 => Ok((channels, SOUND_ENTRY_BYTES)),
        1 => {
            let end = SOUND_ENTRY_BYTES + SOUND_ENTRY_BYTES_OF_VERSION_1;
            entry.get(..end).ok_or(Flaw::Short)?;
            Ok((channels, end))
        }
        2 => {
            let end = SOUND_ENTRY_BYTES + SOUND_ENTRY_BYTES_OF_VERSION_2;
            let fields = entry.get(SOUND_ENTRY_BYTES..end).ok_or(Flaw::Short)?;
            let channels = be32(fields, SOUND_ENTRY_VERSION_2_CHANNELS_AT).ok_or(Flaw::Short)?;
            Ok((u16::try_from(channels).map_err(|_| Flaw::Invalid("channels past 16 bits"))?, end))
        }
        _ => Err(Flaw::Unsupported("a sound description of a version past 2")),
    }
}

impl Soundtrack {
    // Reads the first AAC track from the content of a `moov` box, at `frame` samples a packet; none where the file has no sound track, which is no flaw of a video.
    pub(in crate::codec) fn read(index: &[u8], length: u64, frame: u32) -> Result<Option<Soundtrack>, Flaw> {
        let movie = movie_ticks(index);
        first_track(index, length, |track, length| Soundtrack::read_track(track, length, frame, movie))
    }

    pub(in crate::codec) const fn lead_from(&self, first: usize) -> u32 {
        if first == 0 { self.sound.lead } else { 0 }
    }

    /// The description of a file cut from the packet of that number.
    pub fn cut_from(&self, first: usize) -> Sound {
        Sound { lead: self.lead_from(first), ..self.sound.clone() }
    }

    fn read_track(track: &[u8], length: u64, frame: u32, movie: Option<u32>) -> Result<Option<Soundtrack>, Flaw> {
        let Some(Media { rate, tables, entry: (kind, entry) }) = read_media(track, b"soun")? else {
            return Ok(None);
        };
        if &kind != b"mp4a" {
            return Err(Flaw::Unsupported("sound that is not AAC"));
        }
        let (channels, boxes_at) = read_sound_entry(entry)?;
        let boxes = &entry[boxes_at..];
        // QuickTime may hold the stream's description in a box of its own.
        let boxes =
            if find_box(boxes, b"esds").is_some() { boxes } else { find_box(boxes, WRAPPING_BOX).unwrap_or(boxes) };
        let (stream, _) = read_full_box(boxes, b"esds", "the decoder configuration")?;
        let missing = Flaw::Missing("the decoder configuration");
        let stream = find_descriptor(stream.0, STREAM_DESCRIPTOR).ok_or(missing)?;
        // The stream's fields: its number and flags, and what the flags say follows them.
        let flags = *stream.get(STREAM_FIELDS).ok_or(Flaw::Short)?;
        let mut optional = STREAM_FIELDS + 1;
        if flags & DEPENDS_ON_STREAM != 0 {
            optional += STREAM_REFERENCE_BYTES;
        }
        if flags & ADDRESS_FOLLOWS != 0 {
            optional += 1 + usize::from(*stream.get(optional).ok_or(Flaw::Short)?);
        }
        if flags & CLOCK_STREAM != 0 {
            optional += STREAM_REFERENCE_BYTES;
        }
        let decoder = find_descriptor(stream.get(optional..).ok_or(Flaw::Short)?, DECODER_DESCRIPTOR).ok_or(missing)?;
        if decoder.first() != Some(&MPEG4_AUDIO) {
            return Err(Flaw::Unsupported("sound that is not AAC"));
        }
        let configuration =
            find_descriptor(decoder.get(DECODER_FIELDS..).ok_or(Flaw::Short)?, DECODER_SPECIFICS).ok_or(missing)?;
        let (lead, heard) = read_edits(track, rate, movie)?;
        let (packets, _) = read_samples(tables, length, true)?;
        let sound = Sound { rate, frame, channels, configuration: configuration.to_vec(), lead };
        Ok(Some(Soundtrack { sound, packets, heard }))
    }
}
