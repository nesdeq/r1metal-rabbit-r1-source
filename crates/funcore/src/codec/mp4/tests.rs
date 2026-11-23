use super::read::{
    ADDRESS_FOLLOWS, CLOCK_STREAM, DEPENDS_ON_STREAM, SOUND_ENTRY_BYTES, SOUND_ENTRY_BYTES_OF_VERSION_1,
    SOUND_ENTRY_BYTES_OF_VERSION_2, SOUND_ENTRY_CHANNELS_AT, SOUND_ENTRY_VERSION_2_CHANNELS_AT, SOUND_ENTRY_VERSION_AT,
    VIDEO_ENTRY_BYTES, VIDEO_ENTRY_WIDTH_AT, boxes, parse_box_start, read_descriptor,
};
use super::write::{
    AUDIO_STREAM, LAYER_DESCRIPTOR, NO_LAYER, TRACK_FLAGS, UNIT_16_16, put_box, put_full_box, put_table, put_u16,
    put_u32, put_u32s, put_u64, put_zeros, turn_matrix,
};
use super::*;
use crate::codec::Flaw;
use alloc::vec;
use alloc::vec::Vec;

fn track() -> VideoTrack {
    VideoTrack {
        width: 1280,
        height: 720,
        ticks_per_second: 480_000,
        ticks_per_frame: 7999,
        sequence_parameters: vec![0x67, 0x64, 0x00, 0x29, 0xAC],
        picture_parameters: vec![0x68, 0xEA, 0x43, 0xCB],
        quarters: 1,
        creation_time: 1_791_500_000,
    }
}

fn inside<'a>(bytes: &'a [u8], name: &'static [u8; 4]) -> &'a [u8] {
    super::read::find_box(bytes, name).unwrap_or_else(|| panic!("no box {:?}", core::str::from_utf8(name)))
}

fn path<'a>(mut bytes: &'a [u8], names: &[&'static [u8; 4]]) -> &'a [u8] {
    for name in names {
        bytes = inside(bytes, name);
    }
    bytes
}

fn as_words(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().copied().map(u32::from_be_bytes).collect()
}

fn read(index: &[u8]) -> Result<Movie, Flaw> {
    Movie::read(index, u64::MAX)
}

// The sound track of an index that is read for its sound.
fn read_sound(index: &[u8], length: u64) -> Result<Soundtrack, Flaw> {
    Soundtrack::read(index, length, crate::codec::aac::FRAME as u32)?.ok_or(NO_SOUND_TRACK)
}

fn put(index: &mut [u8], name: &[u8; 4], words: usize, value: u32) {
    let at = index.windows(4).position(|window| window == name).unwrap() + 4 * (1 + words);
    index[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn tables_of(track: &[u8], name: &'static [u8; 4]) -> Vec<u32> {
    as_words(path(track, &[b"mdia", b"minf", b"stbl", name]))
}

fn table_of(index: &[u8], name: &'static [u8; 4]) -> Vec<u32> {
    tables_of(path(index, &[b"moov", b"trak"]), name)
}

fn video_file() -> (Vec<u8>, Tail) {
    let (mut writer, head) = Content::Video(track(), sound()).begin();
    for (index, (bytes, time)) in
        [(1000u32, 0u64), (200, 1), (300, 2), (900, 3), (250, 5), (150, 6)].into_iter().enumerate()
    {
        writer.add_sample(bytes, index % 3 == 0, time * 7999);
    }
    assert_eq!((writer.sample_count(), writer.length(), writer.ticks()), (6, 48 + 2800, 7 * 7999));
    (head, writer.finish())
}

#[test]
fn file_head() {
    let (_, head) = Content::Video(track(), sound()).begin();
    assert_eq!(head.len(), 48);
    assert_eq!(&head[..12], [0, 0, 0, 32, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm']);
    assert_eq!(&head[32..], [0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn tail_appends() {
    let (head, tail) = video_file();
    assert_eq!((tail.size_at, tail.size), (40, 2816u64.to_be_bytes()), "sixteen bytes of header and the samples");
    let (mut file, index) = (head, tail.index.clone());
    file.extend_from_slice(&[0xAA; 2800]);
    assert!(tail.append_to(&mut file));
    let index_at = 48 + 2800;
    assert_eq!(
        (&file[40..48], &file[index_at..]),
        (&2816u64.to_be_bytes()[..], &index[..]),
        "the size stands in the head and the index behind the samples"
    );
    assert_eq!(
        u32::from_be_bytes(file[index_at..index_at + 4].try_into().unwrap()) as usize,
        index.len(),
        "the index is one box"
    );
    assert_eq!(&file[index_at + 4..index_at + 8], b"moov");
    let mut headless = vec![0xAA; 47];
    assert!(
        !video_file().1.append_to(&mut headless) && headless == [0xAA; 47],
        "bytes without the head are left as they are"
    );
}

#[test]
fn sample_tables() {
    let (_, tail) = video_file();
    assert_eq!(table_of(&tail.index, b"stsz"), [0, 0, 6, 1000, 200, 300, 900, 250, 150]);
    assert_eq!(
        table_of(&tail.index, b"stts"),
        [0, 3, 3, 7999, 1, 15998, 2, 7999],
        "the frame before the lost one lasts twice as long"
    );
    assert_eq!(table_of(&tail.index, b"stss"), [0, 2, 1, 4]);
    assert_eq!(table_of(&tail.index, b"stsc"), [0, 1, 1, 3, 1], "every chunk holds one group of three");
    assert_eq!(table_of(&tail.index, b"co64"), [0, 2, 0, 48, 0, 48 + 1500]);
}

#[test]
fn chunk_runs() {
    let (mut writer, _) = Content::Video(track(), sound()).begin();
    for (index, key) in [true, false, true, false, true, true, false, false].into_iter().enumerate() {
        writer.add_sample(10, key, index as u64 * 7999);
    }
    let tail = writer.finish();
    assert_eq!(table_of(&tail.index, b"stsc"), [0, 3, 1, 2, 1, 3, 1, 1, 4, 3, 1]);
    assert_eq!(table_of(&tail.index, b"stts"), [0, 1, 8, 7999]);
    assert_eq!(table_of(&tail.index, b"co64").len(), 2 + 2 * 4);
}

#[test]
fn headers() {
    let (_, tail) = video_file();
    let movie = as_words(path(&tail.index, &[b"moov", b"mvhd"]));
    assert_eq!(movie[0], 1 << 24, "times of 64 bits");
    assert_eq!((u64::from(movie[1]) << 32 | u64::from(movie[2])), 1_791_500_000 + 2_082_844_800);
    assert_eq!((movie[5], movie[6], movie[7]), (480_000, 0, 7 * 7999));
    assert_eq!(*movie.last().unwrap(), 3, "the next free track number, behind the sound's");
    let header = as_words(path(&tail.index, &[b"moov", b"trak", b"tkhd"]));
    assert_eq!(header[0], 1 << 24 | 3);
    assert_eq!((header[5], header[7], header[8]), (1, 0, 7 * 7999));
    let minus = (-65536i32) as u32;
    assert_eq!(
        header[13..22],
        [0, 0x1_0000, 0, minus, 0, 0, 720 << 16, 0, 0x4000_0000],
        "a quarter turn clockwise, moved right by the height"
    );
    assert_eq!(header[22..], [1280 << 16, 720 << 16]);
    let media = as_words(path(&tail.index, &[b"moov", b"trak", b"mdia", b"mdhd"]));
    assert_eq!((media[5], media[7]), (480_000, 7 * 7999));
    assert_eq!(&path(&tail.index, &[b"moov", b"trak", b"mdia", b"hdlr"])[8..12], b"vide");
}

#[test]
fn turn_matrices() {
    let minus = (-65536i32) as u32;
    assert_eq!(turn_matrix(0, 1280, 720), [0x1_0000, 0, 0, 0, 0x1_0000, 0, 0, 0, 0x4000_0000]);
    assert_eq!(turn_matrix(2, 1280, 720), [minus, 0, 0, 0, minus, 0, 1280 << 16, 720 << 16, 0x4000_0000]);
    assert_eq!(turn_matrix(3, 1280, 720), [0, minus, 0, 0x1_0000, 0, 0, 0, 1280 << 16, 0x4000_0000]);
    assert_eq!(turn_matrix(5, 1280, 720), turn_matrix(1, 1280, 720));
}

#[test]
fn sample_description() {
    let (_, tail) = video_file();
    let entry = &path(&tail.index, &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"])[8..];
    assert_eq!(&entry[4..8], b"avc1");
    let fields = &entry[8..];
    assert_eq!((&fields[6..8], &fields[24..28]), (&[0, 1][..], &[5, 0, 2, 0xD0][..]), "data reference 1, 1280 by 720");
    let boxes = &fields[78..];
    let configuration = inside(boxes, b"avcC");
    assert_eq!(
        configuration,
        [
            1, 0x64, 0x00, 0x29, 0xFF, 0xE1, 0, 5, 0x67, 0x64, 0x00, 0x29, 0xAC, 1, 0, 4, 0x68, 0xEA, 0x43, 0xCB, 0xFD,
            0xF8, 0xF8, 0
        ]
    );
    assert_eq!(inside(boxes, b"colr"), [b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0]);
}

#[test]
fn file_without_frames() {
    let (writer, head) = Content::Video(track(), sound()).begin();
    assert_eq!(writer.ticks(), 0);
    let tail = writer.finish();
    assert_eq!(tail.size, 16u64.to_be_bytes());
    assert_eq!(head.len() as u64, tail.size_at + 8);
    assert_eq!(table_of(&tail.index, b"stsz"), [0, 0, 0]);
    assert_eq!(table_of(&tail.index, b"stts"), [0, 0]);
}

#[test]
fn index_round_trip() {
    let (_, tail) = video_file();
    let movie = read(inside(&tail.index, b"moov")).unwrap();
    assert_eq!(
        (movie.width, movie.height, movie.quarters, movie.ticks_per_second, movie.ticks, movie.length_bytes),
        (1280, 720, 1, 480_000, 7 * 7999, 4)
    );
    assert_eq!(
        (&movie.sequence_parameters, &movie.picture_parameters),
        (&vec![track().sequence_parameters], &vec![track().picture_parameters])
    );
    let places: Vec<(u64, u32, u64, bool)> =
        movie.samples.iter().map(|sample| (sample.at, sample.bytes, sample.time / 7999, sample.is_key)).collect();
    assert_eq!(
        places,
        [
            (48, 1000, 0, true),
            (1048, 200, 1, false),
            (1248, 300, 2, false),
            (1548, 900, 3, true),
            (2448, 250, 5, false),
            (2698, 150, 6, false)
        ]
    );
    assert_eq!(movie.frame_ticks(), 7 * 7999 / 6);
    for turns in 0..4 {
        let (mut writer, _) = Content::Video(VideoTrack { quarters: turns, ..track() }, sound()).begin();
        writer.add_sample(10, true, 0);
        assert_eq!(read(inside(&writer.finish().index, b"moov")).unwrap().quarters, turns);
    }
    let (writer, _) = Content::Video(track(), sound()).begin();
    let empty = read(inside(&writer.finish().index, b"moov")).unwrap();
    assert_eq!((empty.samples.len(), empty.ticks, empty.frame_ticks()), (0, 0, 0));
}

fn foreign_index(kind: &[u8; 4], sizes: &[u32], chunks: &[u32]) -> Vec<u8> {
    let mut index = Vec::new();
    put_box(&mut index, b"trak", |out| foreign_media(out, b"soun", [600, 4000], |_| {}));
    put_box(&mut index, b"trak", |out| {
        put_full_box(out, b"tkhd", TRACK_FLAGS, |out| {
            put_zeros(out, 36);
            put_u32s(out, &turn_matrix(3, 640, 360));
        });
        foreign_media(out, b"vide", [600, 4000], |out| {
            put_full_box(out, b"stsd", 0, |out| {
                put_u32(out, 1);
                put_box(out, kind, |out| {
                    put_zeros(out, VIDEO_ENTRY_WIDTH_AT);
                    put_u16(out, 640);
                    put_u16(out, 360);
                    put_zeros(out, VIDEO_ENTRY_BYTES - VIDEO_ENTRY_WIDTH_AT - 4);
                    put_box(out, b"pasp", |out| put_zeros(out, 8));
                    put_box(out, b"avcC", |out| {
                        out.extend_from_slice(&[
                            1, 0x4D, 0x40, 0x1E, 0xFD, 0xE2, 0, 2, 0x67, 0x4D, 0, 1, 0x27, 2, 0, 1, 0x68, 0, 2, 0x68,
                            0xCE,
                        ])
                    });
                });
            });
            put_table(out, b"stts", &[[5, 100]], |out, entry| put_u32s(out, entry));
            put_table(out, b"stsc", &[[1, 2, 1], [2, 3, 1]], |out, entry| put_u32s(out, entry));
            put_full_box(out, b"stsz", 0, |out| {
                put_u32s(out, &[if sizes.is_empty() { 50 } else { 0 }, 5]);
                put_u32s(out, sizes);
            });
            put_table(out, b"co64", chunks, |out, &place| put_u64(out, place.into()));
        });
    });
    index
}

#[test]
fn other_programs_index() {
    let movie = read(&foreign_index(b"avc1", &[], &[1000, 5000])).unwrap();
    assert_eq!(
        (movie.width, movie.height, movie.quarters, movie.ticks_per_second, movie.ticks, movie.length_bytes),
        (640, 360, 3, 600, 500, 2)
    );
    assert_eq!(
        (&movie.sequence_parameters, &movie.picture_parameters),
        (&vec![vec![0x67, 0x4D], vec![0x27]], &vec![vec![0x68], vec![0x68, 0xCE]])
    );
    let places: Vec<(u64, u32, u64, bool)> =
        movie.samples.iter().map(|sample| (sample.at, sample.bytes, sample.time, sample.is_key)).collect();
    assert_eq!(
        places,
        [
            (1000, 50, 0, true),
            (1050, 50, 100, true),
            (5000, 50, 200, true),
            (5050, 50, 300, true),
            (5100, 50, 400, true)
        ],
        "with no table of key samples every sample is one"
    );
    let sized = read(&foreign_index(b"avc1", &[10, 20, 30, 40, 50], &[1000, 5000])).unwrap();
    assert_eq!(sized.samples.iter().map(|sample| sample.at).collect::<Vec<_>>(), [1000, 1010, 5000, 5030, 5070]);
    assert_eq!(sized.frame_ticks(), 100);
}

#[test]
fn unsupported_video() {
    let (_, tail) = video_file();
    let renamed = |from: &[u8; 4], to: &[u8; 4]| {
        let mut index = inside(&tail.index, b"moov").to_vec();
        let at = index.windows(4).position(|window| window == from).unwrap();
        index[at..at + 4].copy_from_slice(to);
        read(&index)
    };
    let unordered = Err(Flaw::Unsupported("samples shown out of the order they are decoded in"));
    assert_eq!(renamed(b"stss", b"ctts"), unordered, "a table of when samples show apart from when they are decoded");
    assert_eq!(renamed(b"co64", b"stco"), Err(Flaw::Missing("the places of the chunks")), "places of 32 bits");
    assert_eq!(renamed(b"avc1", b"avc3"), Err(Flaw::Unsupported("video that is not H.264 in avc1")));
}

#[test]
fn movie_seconds_key_and_times() {
    let sample = |is_key: bool, time: u64| Sample { at: 0, bytes: 1, time, is_key };
    let samples = vec![sample(false, 0), sample(true, 200), sample(true, 100)];
    let movie = Movie { ticks_per_second: 600, ticks: 1799, samples, ..Movie::default() };
    assert_eq!(movie.seconds(), 2, "2.998 seconds are two whole ones");
    assert_eq!(movie.first_key(), Some(1), "a sample before the first key one cannot be started from");
    assert_eq!([movie.time_of(1), movie.time_of(2), movie.time_of(3)], [200, 100, 0], "in the order of decoding");
    let unclocked = Movie { ticks_per_second: 0, samples: vec![sample(false, 0)], ..movie };
    assert_eq!((unclocked.seconds(), unclocked.first_key()), (1799, None), "a clock of none counts a tick a second");
}

#[test]
fn first_sequence_set() {
    use crate::codec::h264::read::samples::OURS;
    use crate::codec::h264::read::samples::X264_SEQUENCE;
    let sets = |first: Vec<u8>, second: Vec<u8>| Movie { sequence_parameters: vec![first, second], ..Movie::default() };
    let level = |movie: Movie| movie.sequence().map(|sequence| (sequence.size(), sequence.level));
    assert_eq!(level(sets(OURS.sequence_parameters(), X264_SEQUENCE.to_vec())), Ok(((1280, 720), 41)));
    assert_eq!(level(sets(X264_SEQUENCE.to_vec(), OURS.sequence_parameters())), Ok(((1280, 720), 40)));
    assert_eq!(Movie::default().sequence(), Err(Flaw::Missing("sequence parameters in the index")));
    assert_eq!(
        sets(vec![0x67], OURS.sequence_parameters()).sequence(),
        Err(Flaw::Short),
        "the first, however it reads"
    );
}

#[test]
fn fixed_size_bounds_count() {
    let index = foreign_index(b"avc1", &[], &[1000, 5000]);
    let samples = |index: &[u8], length: u64| Movie::read(index, length).map(|movie| movie.samples.len());
    assert_eq!(samples(&index, 5150), Ok(5));
    assert_eq!(samples(&index, 149), Ok(2), "two samples of 50 bytes are all a file of 149 can hold");
    let mut greedy = index;
    put(&mut greedy, b"stsz", 2, u32::MAX);
    put(&mut greedy, b"stsc", 3, u32::MAX);
    assert_eq!(samples(&greedy, 5150), Ok(103), "an index that names billions of samples takes memory for 103");
    let mut unbounded = foreign_index(b"avc1", &[], &[1000, 5000]);
    put(&mut unbounded, b"stsz", 2, u32::MAX);
    assert_eq!(
        samples(&unbounded, u64::MAX),
        Err(Flaw::Missing("chunks for all samples")),
        "and one whose chunks hold five takes memory for five"
    );
}

#[test]
fn unplayable_files() {
    assert_eq!(
        read(&foreign_index(b"hvc1", &[], &[1000, 5000])),
        Err(Flaw::Unsupported("video that is not H.264 in avc1"))
    );
    assert_eq!(read(&foreign_index(b"avc1", &[], &[1000])), Err(Flaw::Missing("chunks for all samples")));
    assert_eq!(
        read(&foreign_index(b"avc1", &[1, 2, 3], &[1000, 5000])).map(|movie| movie.samples.len()),
        Ok(3),
        "a table that ends early has that many samples"
    );
    assert_eq!(read(&[]), Err(Flaw::Missing("a video track")));
    let mut sound = Vec::new();
    put_box(&mut sound, b"trak", |out| {
        put_box(out, b"mdia", |out| {
            put_full_box(out, b"hdlr", 0, |out| out.extend_from_slice(b"\0\0\0\0soun"));
        });
    });
    assert_eq!(read(&sound), Err(Flaw::Missing("a video track")));
    let complete = foreign_index(b"avc1", &[], &[1000, 5000]);
    let video_at = u32::from_be_bytes(complete[..4].try_into().unwrap()) as usize;
    let mut index = complete[..complete.len() - 30].to_vec();
    let length = (index.len() - video_at) as u32;
    index[video_at..video_at + 4].copy_from_slice(&length.to_be_bytes());
    assert_eq!(
        read(&index),
        Err(Flaw::Missing("a track's media")),
        "a box that reaches past its parent's end is not there"
    );
}

#[test]
fn box_walk() {
    let mut bytes = Vec::new();
    put_box(&mut bytes, b"ftyp", |out| out.extend_from_slice(b"isom"));
    bytes.extend_from_slice(&[0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 19, 1, 2, 3]);
    bytes.extend_from_slice(&[0, 0, 0, 0, b'f', b'r', b'e', b'e', 9, 9]);
    assert_eq!(parse_box_start(&bytes), Some((*b"ftyp", 12, 8)));
    assert_eq!(parse_box_start(&bytes[12..]), Some((*b"mdat", 19, 16)));
    assert_eq!(
        (parse_box_start(&bytes[12..24]), parse_box_start(&bytes[..7])),
        (None, None),
        "a size of 64 bits needs sixteen bytes to be told"
    );
    let walked: Vec<([u8; 4], &[u8])> = boxes(&bytes).collect();
    assert_eq!(
        walked,
        [(*b"ftyp", &b"isom"[..]), (*b"mdat", &[1, 2, 3]), (*b"free", &[9, 9])],
        "a size of zero reaches to the end"
    );
    assert_eq!(boxes(&[0, 0, 0, 9, b'f', b'r', b'e', b'e']).count(), 0, "a box that does not fit ends the walk");
    assert_eq!(boxes(&[0, 0, 0, 4, b'f', b'r', b'e', b'e']).count(), 0, "as does one smaller than its own header");
}

#[test]
fn find_index_anywhere() {
    let walk = find_index;
    let (mut file, large) = (Vec::new(), [0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 19, 1, 2, 3]);
    put_box(&mut file, b"ftyp", |out| out.extend_from_slice(b"isom"));
    file.extend_from_slice(&large);
    put_box(&mut file, b"moov", |out| out.extend_from_slice(b"index"));
    assert_eq!(walk(&file), Ok(&b"index"[..]), "behind the samples, past a size of 64 bits");
    assert_eq!(seek_index(&file, 0, file.len() as u64), Ok(IndexSeek::Next { at: 12, bytes: 16 }));
    let (mut ahead, mut reaching) = (Vec::new(), file[..31].to_vec());
    put_box(&mut ahead, b"moov", |out| out.extend_from_slice(b"first"));
    ahead.extend_from_slice(&file);
    assert_eq!(walk(&ahead), Ok(&b"first"[..]), "ahead of them");
    reaching.extend_from_slice(&[0, 0, 0, 0, b'm', b'o', b'o', b'v', 7, 7]);
    assert_eq!(walk(&reaching), Ok(&[7, 7][..]), "an index whose size of 0 reaches to the end");
    let mut rest_box = file[..31].to_vec();
    rest_box.extend_from_slice(&[0, 0, 0, 0, b'f', b'r', b'e', b'e', 9]);
    assert_eq!(walk(&rest_box), Err(Flaw::Missing("the index")), "a box of size 0 holds the rest");
    assert_eq!(walk(&file[..31]), Err(Flaw::Missing("the index")), "a file without an index");
    assert_eq!(walk(&[]), Err(Flaw::Missing("the index")), "an empty file");
    assert_eq!(walk(&file[..file.len() - 1]), Err(Flaw::Short), "an index that reaches past the end");
    assert_eq!(walk(&file[..34]), Err(Flaw::Short), "an opening cut short");
    let far = [0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 2, 0, 0, 0, 0];
    assert_eq!(
        seek_index(&far, 8, 1 << 40),
        Ok(IndexSeek::Next { at: (1 << 33) + 8, bytes: 16 }),
        "a box past four gigabytes"
    );
    assert_eq!(seek_index(&far, 8, 1 << 33), Err(Flaw::Short), "a box larger than its file");
    assert_eq!(IndexSeek::start(10), IndexSeek::Next { at: 0, bytes: 10 }, "a file shorter than an opening");
}

#[test]
fn sample_units() {
    let sample = [0, 0, 0, 2, 0x09, 0xF0, 0, 0, 0, 3, 0x65, 0x88, 0x84, 0, 0, 0, 9, 1];
    assert_eq!(
        units(&sample, 4).collect::<Vec<_>>(),
        [&[0x09, 0xF0][..], &[0x65, 0x88, 0x84]],
        "a length the sample has no bytes for ends it"
    );
    assert_eq!(units(&[0, 1, 0x41, 0, 0], 2).collect::<Vec<_>>(), [&[0x41][..], &[]]);
    assert_eq!(units(&[2, 7, 8], 1).collect::<Vec<_>>(), [&[7, 8][..]]);
    assert_eq!(units(&[], 4).count(), 0);
}

fn sound() -> Sound {
    Sound { rate: 48_000, frame: 1024, channels: 1, configuration: vec![0x11, 0x88], lead: 1024 }
}

fn tracks(index: &[u8]) -> Vec<&[u8]> {
    boxes(inside(index, b"moov")).filter(|(name, _)| name == b"trak").map(|(_, track)| track).collect()
}

#[test]
fn sound_file() {
    let (mut writer, head) = Content::Sound(sound(), 1_791_500_000).begin();
    assert_eq!((head.len(), &head[8..12], &head[16..28]), (44, &b"M4A "[..], &b"M4A mp42isom"[..]));
    (0..70).for_each(|_| writer.add_packet(100));
    assert_eq!((writer.packet_count(), writer.sample_count(), writer.length(), writer.ticks()), (70, 0, 44 + 7000, 0));
    let tail = writer.finish();
    assert_eq!((tail.size_at, tail.size), (36, 7016u64.to_be_bytes()));
    let movie = as_words(path(&tail.index, &[b"moov", b"mvhd"]));
    assert_eq!(
        (movie[5], movie[7], *movie.last().unwrap()),
        (48_000, 69 * 1024, 2),
        "the sound's clock, without its lead"
    );
    let track = tracks(&tail.index)[0];
    let header = as_words(inside(track, b"tkhd"));
    assert_eq!((header[5], header[8], header[12]), (1, 69 * 1024, 0x0100_0000), "track 1 at full volume");
    assert_eq!(header[22..], [0, 0], "and of no size on screen");
    assert_eq!(
        as_words(path(track, &[b"edts", b"elst"])),
        [1 << 24, 1, 0, 69 * 1024, 0, 1024, 0x1_0000],
        "played from its lead on, in times of 64 bits"
    );
    let media = as_words(path(track, &[b"mdia", b"mdhd"]));
    assert_eq!((media[5], media[7]), (48_000, 70 * 1024));
    assert_eq!(&path(track, &[b"mdia", b"hdlr"])[8..12], b"soun");
    assert_eq!(path(track, &[b"mdia", b"minf", b"smhd"]), [0; 8]);
    assert_eq!(tables_of(track, b"stts"), [0, 1, 70, 1024]);
    assert_eq!(tables_of(track, b"stsc"), [0, 2, 1, 32, 1, 3, 6, 1], "chunks of 32 packets and what is left");
    assert_eq!(tables_of(track, b"co64"), [0, 3, 0, 44, 0, 44 + 3200, 0, 44 + 6400]);
    assert_eq!(tables_of(track, b"stsz")[..4], [0, 0, 70, 100]);
    let tables = path(track, &[b"mdia", b"minf", b"stbl"]);
    assert!(boxes(tables).all(|(name, _)| &name != b"stss"), "every packet is one a player can start from");
    let entry = &inside(tables, b"stsd")[8..];
    assert_eq!(
        (&entry[4..8], &entry[8 + 16..8 + 20], &entry[8 + 24..8 + 28]),
        (&b"mp4a"[..], &[0, 1, 0, 16][..], &[0xBB, 0x80, 0, 0][..])
    );
    assert_eq!(
        inside(&entry[8 + 28..], b"esds"),
        [
            0, 0, 0, 0, 3, 25, 0, 0, 0, 4, 17, 0x40, 0x15, 0, 3, 0, 0, 0, 0x92, 0x7C, 0, 0, 0x92, 0x7C, 5, 2, 0x11,
            0x88, 6, 1, 2
        ],
        "AAC, a decoder of 768 bytes, 37500 bits a second at most and on average"
    );
}

#[test]
fn sound_round_trip() {
    let (mut writer, _) = Content::Sound(sound(), 0).begin();
    (0..40u32).for_each(|number| writer.add_packet(100 + number));
    let read = read_sound(inside(&writer.finish().index, b"moov"), u64::MAX).unwrap();
    assert_eq!(read.sound, sound());
    assert_eq!(read.packets.len(), 40);
    let place = |number: usize| (read.packets[number].at, read.packets[number].bytes, read.packets[number].time);
    assert_eq!(
        (place(0), place(1), place(39)),
        ((44, 100, 0), (144, 101, 1024), (44 + 39 * 100 + 741, 139, 39 * 1024))
    );
    let (empty, _) = Content::Sound(sound(), 0).begin();
    let read = read_sound(inside(&empty.finish().index, b"moov"), u64::MAX).unwrap();
    assert_eq!((read.packets.len(), read.sound.lead), (0, 1024));
    let long = Sound { configuration: (0..200).map(|byte| byte as u8).collect(), ..sound() };
    let (mut writer, _) = Content::Sound(long.clone(), 0).begin();
    writer.add_packet(100);
    let read = read_sound(inside(&writer.finish().index, b"moov"), u64::MAX).unwrap();
    assert_eq!(
        read.sound.configuration, long.configuration,
        "descriptors too long for a byte of seven bits tell their size in two"
    );
}

#[test]
fn file_starts() {
    let video = Content::Video(track(), sound());
    assert_eq!(
        [video.starts_file(true), video.starts_file(false), Content::Sound(sound(), 0).starts_file(false)],
        [true, false, true],
        "a file of video begins with a frame, one of sound alone with a packet"
    );
    let ((_, video_head), (_, sound_head)) = (video.begin(), Content::Sound(sound(), 1_791_500_000).begin());
    assert_eq!((&video_head[8..12], &sound_head[8..12]), (&b"isom"[..], &b"M4A "[..]));
}

#[test]
fn sample_fits() {
    let sample = Sample { at: 100, bytes: 50, time: 0, is_key: true };
    assert_eq!([sample.fits(150), sample.fits(149)], [true, false]);
    assert!(!Sample { at: u64::MAX, ..sample }.fits(u64::MAX), "nor one whose end no number names");
    let samples = [sample];
    assert_eq!(to_sample(&samples, 0, 150), Ok(&sample));
    assert_eq!(to_sample(&samples, 1, 150), Err("no sample 1".into()));
    assert_eq!(to_sample(&samples, 0, 149), Err("sample 0 reaches past the end of its file".into()));
}

#[test]
fn cut_keeps_lead() {
    let (mut writer, _) = Content::Sound(sound(), 0).begin();
    (0..100).for_each(|_| writer.add_packet(10));
    let track = read_sound(inside(&writer.finish().index, b"moov"), u64::MAX).unwrap();
    assert_eq!([track.lead_from(0), track.lead_from(40)], [1024, 0], "a cut from a later packet keeps no lead");
    assert_eq!(track.cut_from(0), sound(), "the first cut is the sound as it was written");
    assert_eq!(track.cut_from(40), Sound { lead: 0, ..sound() });
}

#[test]
fn interleaved_chunks() {
    let (mut writer, head) = Content::Video(track(), sound()).begin();
    assert_eq!((head.len(), &head[8..12]), (48, &b"isom"[..]));
    writer.add_packet(50);
    writer.add_sample(1000, true, 0);
    writer.add_packet(50);
    writer.add_packet(50);
    writer.add_sample(200, false, 7999);
    writer.add_sample(300, false, 2 * 7999);
    writer.add_packet(50);
    writer.add_sample(900, true, 3 * 7999);
    assert_eq!((writer.sample_count(), writer.packet_count(), writer.length()), (4, 4, 48 + 2600));
    let tail = writer.finish();
    let movie = as_words(path(&tail.index, &[b"moov", b"mvhd"]));
    assert_eq!(
        (movie[5], movie[7], *movie.last().unwrap()),
        (480_000, 4 * 7999, 3),
        "the video's clock and the longer track"
    );
    let (pictures, sounds) = (tracks(&tail.index)[0], tracks(&tail.index)[1]);
    assert_eq!((as_words(inside(pictures, b"tkhd"))[5], as_words(inside(sounds, b"tkhd"))[5]), (1, 2));
    assert_eq!(as_words(inside(sounds, b"tkhd"))[8], 30_720, "three packets after the lead, in the video's ticks");
    assert_eq!(as_words(path(sounds, &[b"edts", b"elst"])), [1 << 24, 1, 0, 30_720, 0, 1024, 0x1_0000]);
    assert_eq!(tables_of(pictures, b"co64"), [0, 3, 0, 98, 0, 1198, 0, 1748], "a frame behind sound begins a chunk");
    assert_eq!(tables_of(pictures, b"stsc"), [0, 3, 1, 1, 1, 2, 2, 1, 3, 1, 1]);
    assert_eq!(tables_of(pictures, b"stss"), [0, 2, 1, 4]);
    assert_eq!(tables_of(sounds, b"co64"), [0, 3, 0, 48, 0, 1098, 0, 1698]);
    assert_eq!(tables_of(sounds, b"stsc"), [0, 3, 1, 1, 1, 2, 2, 1, 3, 1, 1]);
    let index = inside(&tail.index, b"moov");
    let (video, audio) = (read(index).unwrap(), read_sound(index, u64::MAX).unwrap());
    assert_eq!(video.samples.iter().map(|sample| sample.at).collect::<Vec<_>>(), [98, 1198, 1398, 1748]);
    assert_eq!((video.width, video.ticks), (1280, 4 * 7999));
    assert_eq!(
        audio.packets.iter().map(|packet| (packet.at, packet.time)).collect::<Vec<_>>(),
        [(48, 0), (1098, 1024), (1148, 2048), (1698, 3072)]
    );
}

#[test]
fn edit_times() {
    let (mut writer, _) = Content::Video(track(), sound()).begin();
    let packets = (u64::from(u32::MAX) / 10_240 + 2) as usize;
    (0..packets).for_each(|_| writer.add_packet(1));
    let tail = writer.finish();
    let sound_ticks = (packets as u64 * 1024 - 1024) * 10;
    assert!(sound_ticks > u64::from(u32::MAX), "past what 32 bits of the video's ticks hold");
    assert_eq!(
        as_words(path(tracks(&tail.index)[1], &[b"edts", b"elst"])),
        [1 << 24, 1, (sound_ticks >> 32) as u32, sound_ticks as u32, 0, 1024, 0x1_0000]
    );
    assert_eq!(read_sound(inside(&tail.index, b"moov"), u64::MAX).unwrap().sound.lead, 1024);
    let track_box = foreign_sound(b"mp4a", 0x40);
    let edited = |version: u32, entry: &[u32]| {
        let mut index = Vec::new();
        put_box(&mut index, b"trak", |out| {
            out.extend_from_slice(&track_box[BOX_HEADER..]);
            put_box(out, b"edts", |out| {
                put_full_box(out, b"elst", version << 24, |out| {
                    put_u32(out, 1);
                    put_u32s(out, entry);
                })
            });
        });
        read_sound(&index, u64::MAX).map(|track| track.sound.lead)
    };
    assert_eq!(edited(1, &[0, 9000, 0, 2112, UNIT_16_16]), Ok(2112));
    assert_eq!(
        edited(1, &[0, 9000, u32::MAX, u32::MAX, UNIT_16_16]),
        Ok(0),
        "one that begins nowhere, -1, has no lead"
    );
    assert_eq!(edited(1, &[0, 9000, 1, 0, UNIT_16_16]), Ok(0), "and one past 32 bits of samples none");
    assert_eq!(edited(0, &[9000, 2112, UNIT_16_16]), Ok(2112), "an edit of version 0, as other programs write it");
    assert_eq!(edited(0, &[9000, u32::MAX, UNIT_16_16]), Ok(0), "which begins nowhere with -1 of 32 bits");
    assert_eq!(edited(0, &[9000, 0, UNIT_16_16]), Ok(0), "or at the first sample");
    let mut future = Vec::new();
    put_box(&mut future, b"trak", |out| {
        out.extend_from_slice(&track_box[BOX_HEADER..]);
        put_box(out, b"edts", |out| put_full_box(out, b"elst", 2 << 24, |out| put_u32(out, 0)));
    });
    assert_eq!(
        read_sound(&future, u64::MAX),
        Err(Flaw::Unsupported("an edit list of a version past 1")),
        "and a version there is none of is not guessed at"
    );
}

fn foreign_sound(kind: &[u8; 4], object: u8) -> Vec<u8> {
    let mut index = Vec::new();
    put_box(&mut index, b"trak", |out| {
        foreign_media(out, b"soun", [44_100, 9000], |out| {
            put_full_box(out, b"stsd", 0, |out| {
                put_u32(out, 1);
                put_box(out, kind, |out| {
                    put_zeros(out, SOUND_ENTRY_CHANNELS_AT);
                    put_u16(out, 2);
                    put_zeros(out, SOUND_ENTRY_BYTES - SOUND_ENTRY_CHANNELS_AT - 2);
                    put_full_box(out, b"esds", 0, |out| {
                        out.extend_from_slice(&[3, 0x80, 0x80, 0x80, 37, 0, 1, 0]);
                        out.extend_from_slice(&[
                            4, 0x80, 0x80, 0x80, 23, object, 0x15, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0,
                        ]);
                        out.extend_from_slice(&[5, 0x80, 0x80, 0x80, 5, 0x12, 0x10, 0x56, 0xE5, 0]);
                        out.extend_from_slice(&[6, 0x80, 0x80, 0x80, 1, 2]);
                    });
                });
            });
            put_table(out, b"stts", &[[3, 1024]], |out, entry| put_u32s(out, entry));
            put_table(out, b"stsc", &[[1, 3, 1]], |out, entry| put_u32s(out, entry));
            put_full_box(out, b"stsz", 0, |out| put_u32s(out, &[0, 3, 10, 20, 30]));
            put_table(out, b"co64", &[500], |out, &place| put_u64(out, place));
        });
    });
    index
}

fn foreign_media(out: &mut Vec<u8>, handler: &[u8; 4], [rate, ticks]: [u32; 2], tables: impl FnOnce(&mut Vec<u8>)) {
    put_box(out, b"mdia", |out| {
        put_full_box(out, b"mdhd", 0, |out| put_u32s(out, &[0, 0, rate, ticks, 0]));
        put_full_box(out, b"hdlr", 0, |out| {
            put_zeros(out, 4);
            out.extend_from_slice(handler);
        });
        put_box(out, b"minf", |out| put_box(out, b"stbl", tables));
    });
}

#[test]
fn other_programs_sound() {
    let read = read_sound(&foreign_sound(b"mp4a", 0x40), u64::MAX).unwrap();
    assert_eq!(
        read.sound,
        Sound { rate: 44_100, frame: 1024, channels: 2, configuration: vec![0x12, 0x10, 0x56, 0xE5, 0], lead: 0 },
        "no list of edits, no lead"
    );
    assert_eq!(
        read.packets.iter().map(|packet| (packet.at, packet.bytes)).collect::<Vec<_>>(),
        [(500, 10), (510, 20), (530, 30)]
    );
    let unsupported = Err(Flaw::Unsupported("sound that is not AAC"));
    assert_eq!(read_sound(&foreign_sound(b"ac-3", 0x40), u64::MAX), unsupported);
    assert_eq!(read_sound(&foreign_sound(b"mp4a", 0x6B), u64::MAX), unsupported, "MP3 in the same box");
    let index = Content::Video(track(), sound()).begin().0.finish().index;
    let mut video = Vec::new();
    put_box(&mut video, b"trak", |out| out.extend_from_slice(tracks(&index)[0]));
    assert_eq!(
        Soundtrack::read(&video, u64::MAX, 1024),
        Ok(None),
        "a file of video alone has no sound track, which is no flaw of it"
    );
    assert_eq!(read_sound(&video, u64::MAX), Err(NO_SOUND_TRACK), "and is one where the file is read for its sound");
    let mut both = foreign_sound(b"ac-3", 0x40);
    both.extend_from_slice(&foreign_sound(b"mp4a", 0x40));
    assert_eq!(
        Soundtrack::read(&both, u64::MAX, 1024).map(|track| track.map(|track| track.packets.len())),
        Ok(Some(3)),
        "a sound track that cannot be read is passed over for one that can"
    );
    let mut cut = foreign_sound(b"mp4a", 0x40);
    let at = cut.windows(5).position(|window| window == [3, 0x80, 0x80, 0x80, 37]).unwrap();
    cut[at + 4] = 3;
    assert_eq!(
        read_sound(&cut, u64::MAX),
        Err(Flaw::Missing("the decoder configuration")),
        "a stream description that ends before the decoder's part"
    );
    cut[at + 4 + 1 + STREAM_FIELDS] = DEPENDS_ON_STREAM | CLOCK_STREAM;
    assert_eq!(
        read_sound(&cut, u64::MAX),
        Err(Flaw::Short),
        "one that depends on another stream and has a clock, with no room for them"
    );
    assert_eq!(read_descriptor(&[5, 0x81, 0x02, 9]), None, "a descriptor of 130 bytes in one");
    assert_eq!(read_descriptor(&[5, 2, 8, 9, 7]), Some((5, &[8, 9][..], &[7][..])));
}

// A descriptor with that tag whose size is told in exactly that many bytes of seven bits, with the content `fill` writes.
fn put_descriptor_in(out: &mut Vec<u8>, tag: u8, bytes: usize, fill: impl FnOnce(&mut Vec<u8>)) {
    out.push(tag);
    let start = out.len();
    fill(out);
    let size = out.len() - start;
    let sized: Vec<u8> = (0..bytes)
        .rev()
        .map(|byte| {
            let bits = (size >> (SIZE_BITS as usize * byte)) as u8 & !SIZE_GOES_ON;
            if byte == 0 { bits } else { bits | SIZE_GOES_ON }
        })
        .collect();
    out.splice(start..start, sized);
}

// The stream description of a sound track as phones write it: its descriptors' sizes told in `size_bytes` bytes each, the stream's flags and the fields they say follow, and the decoder's configuration.
fn put_stream_description(out: &mut Vec<u8>, size_bytes: usize, flags: u8, optional: &[u8], configuration: &[u8]) {
    put_full_box(out, b"esds", 0, |out| {
        put_descriptor_in(out, STREAM_DESCRIPTOR, size_bytes, |out| {
            out.extend_from_slice(&[0, 1, flags]);
            out.extend_from_slice(optional);
            put_descriptor_in(out, DECODER_DESCRIPTOR, size_bytes, |out| {
                out.extend_from_slice(&[MPEG4_AUDIO, AUDIO_STREAM, 0, 0, 0]);
                put_u32s(out, &[64_000, 48_000]);
                put_descriptor_in(out, DECODER_SPECIFICS, size_bytes, |out| out.extend_from_slice(configuration));
            });
            put_descriptor_in(out, LAYER_DESCRIPTOR, size_bytes, |out| out.push(NO_LAYER));
        });
    });
}

// Low complexity AAC, 44.1 kHz, two channels.
const CONFIGURATION_44100: [u8; 2] = [0x12, 0x10];

// An index of one sound track at 44.1 kHz as phones write it: the sample entry of that version with the boxes `entry` writes behind its fields, the tables `tables` writes, the boxes `before` writes ahead of the track's media, and the movie's header with that many ticks a second where there is a number.
fn phone_sound(
    version: u16,
    entry: impl FnOnce(&mut Vec<u8>),
    tables: impl FnOnce(&mut Vec<u8>),
    before: impl FnOnce(&mut Vec<u8>),
    movie: Option<u32>,
) -> Vec<u8> {
    let mut index = Vec::new();
    if let Some(ticks) = movie {
        put_full_box(&mut index, b"mvhd", 0, |out| put_u32s(out, &[0, 0, ticks, 0]));
    }
    put_box(&mut index, b"trak", |out| {
        before(out);
        foreign_media(out, b"soun", [44_100, 9000], |out| {
            put_full_box(out, b"stsd", 0, |out| {
                put_u32(out, 1);
                put_box(out, b"mp4a", |out| {
                    put_zeros(out, SOUND_ENTRY_VERSION_AT);
                    put_u16(out, version);
                    put_zeros(out, SOUND_ENTRY_CHANNELS_AT - SOUND_ENTRY_VERSION_AT - 2);
                    put_u16(out, if version == 2 { 3 } else { 2 });
                    put_zeros(out, SOUND_ENTRY_BYTES - SOUND_ENTRY_CHANNELS_AT - 2);
                    match version {
                        1 => put_zeros(out, SOUND_ENTRY_BYTES_OF_VERSION_1),
                        2 => {
                            put_zeros(out, SOUND_ENTRY_VERSION_2_CHANNELS_AT);
                            put_u32(out, 2);
                            put_zeros(out, SOUND_ENTRY_BYTES_OF_VERSION_2 - SOUND_ENTRY_VERSION_2_CHANNELS_AT - 4);
                        }
                        _ => {}
                    }
                    entry(out);
                });
            });
            tables(out);
        });
    });
    index
}

// The tables of three chunks of 2, 1 and 3 samples of sound, in two runs as the standard has them, with the places of the chunks in the box `places` writes.
fn phone_tables(out: &mut Vec<u8>, places: impl FnOnce(&mut Vec<u8>)) {
    put_table(out, b"stts", &[[6, 1024]], |out, entry| put_u32s(out, entry));
    put_table(out, b"stsc", &[[1, 2, 1], [2, 1, 1], [3, 3, 1]], |out, entry| put_u32s(out, entry));
    put_full_box(out, b"stsz", 0, |out| put_u32s(out, &[0, 6, 10, 20, 30, 40, 50, 60]));
    places(out);
}

fn places_in_32(out: &mut Vec<u8>) {
    put_table(out, b"stco", &[100u32, 300, 500], |out, &place| put_u32(out, place));
}

#[test]
fn chunk_place_widths() {
    let places_in_64 =
        |out: &mut Vec<u8>| put_table(out, b"co64", &[100u64, 300, 500], |out, &place| put_u64(out, place));
    let found = |places: &dyn Fn(&mut Vec<u8>), is_reordered: bool| {
        let index = phone_sound(
            0,
            |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100),
            |out| {
                phone_tables(out, places);
                if is_reordered {
                    put_table(out, b"ctts", &[[6, 0]], |out, entry| put_u32s(out, entry));
                }
            },
            |_| {},
            None,
        );
        read_sound(&index, u64::MAX).map(|track| {
            (
                track.sound.configuration,
                track.packets.iter().map(|packet| (packet.at, packet.bytes, packet.time)).collect::<Vec<_>>(),
            )
        })
    };
    let expected = (
        CONFIGURATION_44100.to_vec(),
        vec![(100, 10, 0), (110, 20, 1024), (300, 30, 2048), (500, 40, 3072), (540, 50, 4096), (590, 60, 5120)],
    );
    assert_eq!(found(&places_in_32, false), Ok(expected.clone()), "places of 32 bits, as every phone writes them");
    assert_eq!(found(&places_in_64, false), Ok(expected.clone()), "and of 64");
    assert_eq!(
        found(&places_in_32, true),
        Ok(expected),
        "a table of times shown is of no use to sound: it is in order"
    );
    let neither = phone_sound(
        0,
        |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100),
        |out| phone_tables(out, |_| {}),
        |_| {},
        None,
    );
    assert_eq!(read_sound(&neither, u64::MAX), Err(Flaw::Missing("the places of the chunks")));
}

#[test]
fn stream_descriptor_sizes_and_optional_fields() {
    let url = [3, b'u', b'r', b'l'];
    for (size_bytes, flags, optional) in [
        (1, 0u8, vec![]),
        (2, 0, vec![]),
        (3, 0, vec![]),
        (4, 0, vec![]),
        (4, DEPENDS_ON_STREAM, vec![0, 7]),
        (1, ADDRESS_FOLLOWS, url.to_vec()),
        (2, CLOCK_STREAM, vec![0, 9]),
        (3, DEPENDS_ON_STREAM | ADDRESS_FOLLOWS | CLOCK_STREAM, [&[0, 7][..], &url, &[0, 9]].concat()),
    ] {
        let index = phone_sound(
            0,
            |out| put_stream_description(out, size_bytes, flags, &optional, &CONFIGURATION_44100),
            |out| phone_tables(out, places_in_32),
            |_| {},
            None,
        );
        let track =
            read_sound(&index, u64::MAX).unwrap_or_else(|why| panic!("{size_bytes} bytes, flags {flags:#x}: {why:?}"));
        assert_eq!(track.sound.configuration, CONFIGURATION_44100, "{size_bytes} bytes, flags {flags:#x}");
    }
    let index = phone_sound(
        0,
        |out| put_stream_description(out, 1, ADDRESS_FOLLOWS, &[200, 1, 2], &CONFIGURATION_44100),
        |out| phone_tables(out, places_in_32),
        |_| {},
        None,
    );
    assert_eq!(
        read_sound(&index, u64::MAX).map(|_| ()),
        Err(Flaw::Short),
        "an address longer than the descriptor that holds it"
    );
}

#[test]
fn entry_versions_and_wave_box() {
    let tables = |out: &mut Vec<u8>| phone_tables(out, places_in_32);
    let direct =
        phone_sound(1, |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100), tables, |_| {}, None);
    let wrapped = phone_sound(
        1,
        |out| {
            put_box(out, b"wave", |out| {
                put_box(out, b"frma", |out| out.extend_from_slice(b"mp4a"));
                put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100);
                put_box(out, b"\0\0\0\0", |_| {});
            });
        },
        tables,
        |_| {},
        None,
    );
    let second =
        phone_sound(2, |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100), tables, |_| {}, None);
    let second_wrapped = phone_sound(
        2,
        |out| put_box(out, b"wave", |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100)),
        tables,
        |_| {},
        None,
    );
    for (name, index) in [
        ("version 1 with the stream behind its fields", direct),
        ("version 1 with the stream in a box of its own", wrapped),
        ("version 2, whose channels stand in a field of 32 bits", second),
        ("version 2 with the stream in a box of its own", second_wrapped),
    ] {
        let track = read_sound(&index, u64::MAX).unwrap_or_else(|why| panic!("{name}: {why:?}"));
        assert_eq!((track.sound.channels, track.sound.configuration), (2, CONFIGURATION_44100.to_vec()), "{name}");
    }
    let third =
        phone_sound(3, |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100), tables, |_| {}, None);
    assert_eq!(
        read_sound(&third, u64::MAX).map(|_| ()),
        Err(Flaw::Unsupported("a sound description of a version past 2"))
    );
    let absent = phone_sound(1, |out| put_box(out, b"wave", |_| {}), tables, |_| {}, None);
    assert_eq!(read_sound(&absent, u64::MAX).map(|_| ()), Err(Flaw::Missing("the decoder configuration")));
    let mut vast =
        phone_sound(2, |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100), tables, |_| {}, None);
    let at = vast.windows(4).position(|name| name == b"mp4a").unwrap()
        + 4
        + SOUND_ENTRY_BYTES
        + SOUND_ENTRY_VERSION_2_CHANNELS_AT;
    vast[at..at + 4].copy_from_slice(&(u32::from(u16::MAX) + 1).to_be_bytes());
    assert_eq!(read_sound(&vast, u64::MAX).map(|_| ()), Err(Flaw::Invalid("channels past 16 bits")));
    for version in [1, 2] {
        let mut index = Vec::new();
        put_box(&mut index, b"trak", |out| {
            foreign_media(out, b"soun", [44_100, 9000], |out| {
                put_full_box(out, b"stsd", 0, |out| {
                    put_u32(out, 1);
                    put_box(out, b"mp4a", |out| {
                        put_zeros(out, SOUND_ENTRY_VERSION_AT);
                        put_u16(out, version);
                        put_zeros(
                            out,
                            SOUND_ENTRY_BYTES - SOUND_ENTRY_VERSION_AT - 2 + SOUND_ENTRY_VERSION_2_CHANNELS_AT,
                        );
                    });
                });
            });
        });
        assert_eq!(
            read_sound(&index, u64::MAX).map(|_| ()),
            Err(Flaw::Short),
            "an entry of version {version} that ends before the fields of its version do"
        );
    }
}

#[test]
fn edit_list_start_and_length() {
    let tables = |out: &mut Vec<u8>| phone_tables(out, places_in_32);
    let edit = |version: u32, entries: &[&[u32]], movie: Option<u32>| {
        let index = phone_sound(
            0,
            |out| put_stream_description(out, 1, 0, &[], &CONFIGURATION_44100),
            tables,
            |out| {
                put_box(out, b"edts", |out| {
                    put_full_box(out, b"elst", version << 24, |out| {
                        put_u32(out, entries.len() as u32);
                        entries.iter().for_each(|entry| put_u32s(out, entry));
                    })
                })
            },
            movie,
        );
        read_sound(&index, u64::MAX).map(|track| (track.sound.lead, track.heard))
    };
    let unit = UNIT_16_16;
    // The movie ticks 1000 a second, the media 44100: an edit of 2000 ticks is 88200 samples.
    assert_eq!(edit(0, &[&[2000, 1024, unit]], Some(1000)), Ok((1024, Some(88_200))));
    assert_eq!(
        edit(0, &[&[500, u32::MAX, unit], &[2000, 1024, unit]], Some(1000)),
        Ok((1024, Some(88_200))),
        "an empty edit before it"
    );
    assert_eq!(edit(1, &[&[0, 2000, 0, 1024, unit]], Some(1000)), Ok((1024, Some(88_200))), "in times of 64 bits");
    assert_eq!(
        edit(0, &[&[1, 0, unit]], Some(1000)),
        Ok((0, Some(45))),
        "a tick of the movie's is 44.1 samples of the sound's, rounded up"
    );
    assert_eq!(
        edit(0, &[&[2000, 1024, unit], &[1000, 5000, unit]], Some(1000)),
        Ok((1024, None)),
        "edits that play sound apart set no end"
    );
    assert_eq!(edit(0, &[&[2000, 1024, unit]], None), Ok((1024, None)), "nor does an index that tells no movie clock");
    assert_eq!(
        edit(0, &[&[2000, 1024, unit]], Some(0)),
        Ok((1024, Some(88_200 * 1000))),
        "a clock of none counts a tick a second"
    );
    assert_eq!(edit(0, &[&[500, u32::MAX, unit]], Some(1000)), Ok((0, None)), "an edit list of nothing but waiting");
    assert_eq!(edit(0, &[], Some(1000)), Ok((0, None)), "and one of no edits");
}

#[test]
#[ignore]
fn ffmpeg_plays_sound() {
    extern crate std;
    use crate::codec::aac::CONFIGURATION;
    use crate::codec::aac::DELAY;
    use crate::codec::aac::Encoder;
    use crate::codec::aac::FRAME;
    use crate::codec::aac::RATE;
    use std::process::Command;
    let tone = |at: usize, hertz: f64| crate::base::math::sine_cosine(hertz * at as f64 / f64::from(RATE)).0;
    let wave: Vec<i16> =
        (0..50 * FRAME).map(|at| (8000.0 * tone(at, 440.0) + 3000.0 * tone(at, 2000.0)) as i16).collect();
    let lead = DELAY as u32;
    let sound = Sound { rate: RATE, frame: FRAME as u32, channels: 1, configuration: CONFIGURATION.to_vec(), lead };
    let (mut writer, mut file) = Content::Sound(sound, 1_791_500_000).begin();
    let (mut encoder, mut packet) = (Encoder::new(128_000), Vec::new());
    for frame in wave.as_chunks::<FRAME>().0.iter().chain([&[0i16; FRAME]]) {
        encoder.encode(frame, &mut packet);
        writer.add_packet(packet.len() as u32);
        file.extend_from_slice(&packet);
    }
    assert!(writer.finish().append_to(&mut file));
    let folder = std::env::temp_dir().join(std::format!("r1metal-m4a-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let (note, raw) = (folder.join("note.m4a"), folder.join("played.raw"));
    std::fs::write(&note, &file).unwrap();
    let output = Command::new("ffmpeg")
        .args(["-v", "warning", "-y", "-i"])
        .arg(&note)
        .args(["-f", "s16le", "-c:a", "pcm_s16le"])
        .arg(&raw)
        .output()
        .expect("ffmpeg on the host");
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{}",
        std::string::String::from_utf8_lossy(&output.stderr)
    );
    let decoded: Vec<i16> =
        std::fs::read(&raw).unwrap().as_chunks::<2>().0.iter().copied().map(i16::from_le_bytes).collect();
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(decoded.len(), wave.len(), "the lead is passed over and the last frame flushed");
    let noise: f64 =
        wave.iter().zip(&decoded).map(|(&input, &output)| (f64::from(input) - f64::from(output)).powi(2)).sum();
    let signal: f64 = wave.iter().map(|&input| f64::from(input).powi(2)).sum();
    assert!(signal / noise > 100.0, "what is played lies {} times under what was recorded", signal / noise);
}
