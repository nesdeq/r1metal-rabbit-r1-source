//! AAC low complexity: an encoder for r1metal's own sound, mono at 48 kHz in long sine windows; a decoder for that and for what phones record, one channel or two at any rate of the standard; ADTS streams.

mod adts;
mod decode;
mod encode;
mod tables;
mod tools;
mod transform;

pub use decode::Decoder;
pub(crate) use encode::Encoder;

use crate::base::bits::{BitReader, BitWriter};
use crate::base::math::pow2;
use crate::codec::Flaw;
use crate::codec::mp4::{NO_SOUND_TRACK, Soundtrack, find_index};
use alloc::vec::Vec;
use tables::{BOOK_1, BOOK_2, BOOK_3, BOOK_4, BOOK_5, BOOK_6, BOOK_7, BOOK_8, BOOK_9, BOOK_10, BOOK_11, RATES, split};

/// Samples per packet.
pub const FRAME: usize = 1024;
pub const RATE: u32 = 48_000;
/// Samples a decoder gives before the first encoded sample.
pub const DELAY: usize = FRAME;
pub(crate) const CONFIGURATION: [u8; 2] = [0x11, 0x88];
const LARGEST_PACKET: usize = 768;

const BANDS: usize = tables::BANDS.len() - 1;
// The elements of a packet, in the three bits each begins with (4.4.2.1).
const SINGLE_CHANNEL: u32 = 0;
const CHANNEL_PAIR: u32 = 1;
const COUPLING_CHANNEL: u32 = 2;
const LOW_FREQUENCY: u32 = 3;
const DATA_STREAM: u32 = 4;
const PROGRAM_CONFIG: u32 = 5;
const FILL: u32 = 6;
const END_ELEMENT: u32 = 7;
const ELEMENT_BITS: u32 = 3;
const TAG_BITS: u32 = 4;
const GAIN_BITS: u32 = 8;
const SEQUENCE_BITS: u32 = 2;
const BAND_COUNT_BITS: u32 = 6;
const BOOK_BITS: u32 = 4;
const RUN_BITS: u32 = 5;
const RUN_ESCAPE: u32 = (1 << RUN_BITS) - 1;
const UNIT_SCALE: i32 = 100;
const SCALE_REACH: i32 = 60;
const SCALES: core::ops::RangeInclusive<i32> = 0..=255;
const ZERO_BOOK: usize = 0;
const BOOKS: usize = 11;
const RESERVED_BOOK: usize = BOOKS + 1;
// 4.6.13 and 4.6.8.2: a band of noise, and a band of intensity out of phase and in phase.
const NOISE_BOOK: usize = RESERVED_BOOK + 1;
const OUT_OF_PHASE_BOOK: usize = NOISE_BOOK + 1;
const IN_PHASE_BOOK: usize = OUT_OF_PHASE_BOOK + 1;
const ESCAPE: u32 = 16;
const ESCAPE_BITS: u32 = 4;
const LARGEST_VALUE: u32 = 8191;
const LARGEST_GROUP: usize = 4;
const OCTAVE_ROOT: f64 = 1.044_273_782_427_413_8;
const OCTAVE_STEPS: i32 = 16;
const SCALE_STEPS: i32 = 4;

// Bits of an AudioSpecificConfig's object type, rate index and channel configuration (1.6.2.1).
const OBJECT_BITS: u32 = 5;
const RATE_INDEX_BITS: u32 = 4;
const CHANNEL_CONFIG_BITS: u32 = 4;
const FLAG_BITS: u32 = 3;
const LOW_COMPLEXITY: u32 = 2;
const EXPLICIT_RATE: u32 = 15;
const EXPLICIT_RATE_BITS: u32 = 24;
// What begins an extension behind a configuration: its sync word, its object type, and the object type of spectral band replication.
const EXTENSION_SYNC: u32 = 0x2B7;
const EXTENSION_SYNC_BITS: u32 = 11;
const SBR_OBJECT: u32 = 5;

pub(crate) fn seconds(packets: usize, lead: usize, rate: u32) -> u64 {
    ((packets * FRAME).saturating_sub(lead) as u64).div_ceil(u64::from(rate.max(1)))
}

/// Whole seconds of `count` packets from packet `first`; only packet 0 has the lead to pass over.
pub fn cut_seconds(track: &Soundtrack, first: usize, count: usize) -> u64 {
    seconds(count, track.lead_from(first) as usize, track.sound.rate)
}

// The samples a track plays: its packets' less the lead, and no more than its edit lets be heard.
pub(crate) fn track_samples(track: &Soundtrack) -> u64 {
    let packets = (track.packets.len() * FRAME).saturating_sub(track.sound.lead as usize) as u64;
    track.heard.map_or(packets, |heard| heard.min(packets))
}

/// Whole seconds of a track.
pub fn track_seconds(track: &Soundtrack) -> u64 {
    track_samples(track).div_ceil(u64::from(track.sound.rate.max(1)))
}

/// The AAC track of an MP4 file `length` bytes long, by its index; none where the file has no sound track.
pub fn read_index(index: &[u8], length: u64) -> Result<Option<Soundtrack>, Flaw> {
    Soundtrack::read(index, length, FRAME as u32)
}

// The sound of a file held whole: a stream of ADTS, else an MP4 file.
pub(crate) fn read_track(file: &[u8]) -> Result<Soundtrack, Flaw> {
    if adts::begins(file) {
        adts::read(file)
    } else {
        read_index(find_index(file)?, file.len() as u64)?.ok_or(NO_SOUND_TRACK)
    }
}

// What a decoder has to be told of a stream, from the AudioSpecificConfig: the index of its rate and its channels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Config {
    index: usize,
    channels: usize,
}

impl Config {
    // Our own stream: 48 kHz, one channel.
    const OURS: Config = Config { index: 3, channels: 1 };

    pub(crate) fn rate(self) -> u32 {
        RATES[self.index]
    }

    pub(crate) const fn channels(self) -> usize {
        self.channels
    }

    // The bytes of a configuration of that object type, rate index and channel configuration, with its flags clear.
    fn bytes(object: u32, index: u32, channels: u32) -> Vec<u8> {
        let mut bits = BitWriter::default();
        bits.put(object, OBJECT_BITS);
        bits.put(index, RATE_INDEX_BITS);
        bits.put(channels, CHANNEL_CONFIG_BITS);
        bits.put(0, FLAG_BITS);
        bits.finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Config, Flaw> {
        let mut reader = Reader(BitReader::new(bytes));
        if reader.take(OBJECT_BITS)? != LOW_COMPLEXITY {
            return Err(Flaw::Unsupported("an object type that is not low complexity"));
        }
        let index = match reader.take(RATE_INDEX_BITS)? {
            EXPLICIT_RATE => {
                let rate = reader.take(EXPLICIT_RATE_BITS)?;
                RATES
                    .iter()
                    .position(|&known| known == rate)
                    .ok_or(Flaw::Unsupported("a sampling frequency without an index"))?
            }
            index if (index as usize) < RATES.len() => index as usize,
            _ => return Err(Flaw::Broken),
        };
        let channels = match reader.take(CHANNEL_CONFIG_BITS)? {
            0 => return Err(Flaw::Unsupported("a channel layout of the stream's own")),
            1 => 1,
            2 => 2,
            _ => return Err(Flaw::Unsupported("more than two channels")),
        };
        for what in ["frames of 960 samples", "a core coder", "an extended configuration"] {
            if reader.bit()? != 0 {
                return Err(Flaw::Unsupported(what));
            }
        }
        let is_extended = reader.0.remaining() >= (EXTENSION_SYNC_BITS + OBJECT_BITS) as usize
            && reader.peek(EXTENSION_SYNC_BITS)? == EXTENSION_SYNC;
        if is_extended {
            reader.take(EXTENSION_SYNC_BITS)?;
            if reader.take(OBJECT_BITS)? == SBR_OBJECT && reader.bit()? != 0 {
                return Err(Flaw::Unsupported("spectral band replication"));
            }
        }
        Ok(Config { index, channels })
    }
}

struct Book {
    codes: &'static [u32],
    group: usize,
    largest: u32,
    is_signed: bool,
}

impl Book {
    const fn base(&self) -> u32 {
        if self.is_signed { 2 * self.largest + 1 } else { self.largest + 1 }
    }

    fn index(&self, group: &[i32]) -> usize {
        let base = self.base();
        group.iter().fold(0, |index, &value| {
            let digit = if self.is_signed {
                (value + self.largest as i32) as u32
            } else {
                value.unsigned_abs().min(self.largest)
            };
            index * base + digit
        }) as usize
    }

    fn code(&self, index: usize) -> (u32, u32) {
        split(self.codes[index])
    }
}

static CODE_BOOKS: [Book; BOOKS] = [
    Book { codes: &BOOK_1, group: 4, largest: 1, is_signed: true },
    Book { codes: &BOOK_2, group: 4, largest: 1, is_signed: true },
    Book { codes: &BOOK_3, group: 4, largest: 2, is_signed: false },
    Book { codes: &BOOK_4, group: 4, largest: 2, is_signed: false },
    Book { codes: &BOOK_5, group: 2, largest: 4, is_signed: true },
    Book { codes: &BOOK_6, group: 2, largest: 4, is_signed: true },
    Book { codes: &BOOK_7, group: 2, largest: 7, is_signed: false },
    Book { codes: &BOOK_8, group: 2, largest: 7, is_signed: false },
    Book { codes: &BOOK_9, group: 2, largest: 12, is_signed: false },
    Book { codes: &BOOK_10, group: 2, largest: 12, is_signed: false },
    Book { codes: &BOOK_11, group: 2, largest: ESCAPE, is_signed: false },
];

fn band(number: usize) -> core::ops::Range<usize> {
    usize::from(tables::BANDS[number])..usize::from(tables::BANDS[number + 1])
}

fn escape_ones(value: u32) -> u32 {
    u32::BITS - 1 - value.leading_zeros() - ESCAPE_BITS
}

// Two to the power of each step of an octave.
static POWERS: [f32; OCTAVE_STEPS as usize] = {
    let (mut table, mut power, mut step) = ([0.0; OCTAVE_STEPS as usize], 1.0f64, 0);
    while step < table.len() {
        table[step] = power as f32;
        power *= OCTAVE_ROOT;
        step += 1;
    }
    table
};

fn power(steps: i32) -> f32 {
    POWERS[steps.rem_euclid(OCTAVE_STEPS) as usize] * pow2(steps.div_euclid(OCTAVE_STEPS))
}

struct Reader<'a>(BitReader<'a>);

impl Reader<'_> {
    fn bit(&mut self) -> Result<u32, Flaw> {
        self.0.bit().ok_or(Flaw::Short)
    }

    fn take(&mut self, count: u32) -> Result<u32, Flaw> {
        self.0.take(count).ok_or(Flaw::Short)
    }

    // The next `count` bits, up to 32, left to be read.
    fn peek(&self, count: u32) -> Result<u32, Flaw> {
        self.0.clone().take(count).ok_or(Flaw::Short)
    }

    fn skip(&mut self, bits: usize) -> Result<(), Flaw> {
        self.0.skip(bits).ok_or(Flaw::Short)
    }

    // Passes over the bits that end the byte the reader stands in.
    fn align(&mut self) -> Result<(), Flaw> {
        let byte = u8::BITS as usize;
        self.skip((byte - self.0.position() % byte) % byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn books_prefix_free() {
        let books = CODE_BOOKS.iter().map(|book| book.codes).chain([&tables::SCALEFACTORS[..]]);
        for (number, codes) in books.enumerate() {
            let mut words: Vec<(u32, u32)> = codes.iter().map(|&entry| split(entry)).collect();
            let longest = words.iter().map(|&(_, length)| length).max().unwrap();
            let coverage: u64 = words.iter().map(|&(_, length)| 1u64 << (longest - length)).sum();
            assert_eq!(coverage, 1 << longest, "book {} leaves no bits unused and none used twice", number + 1);
            words.sort_by_key(|&(code, length)| u64::from(code) << (longest - length));
            for pair in words.windows(2) {
                let ((first, first_bits), (second, second_bits)) = (pair[0], pair[1]);
                let prefix = first_bits.min(second_bits);
                assert_ne!(first >> (first_bits - prefix), second >> (second_bits - prefix), "book {}", number + 1);
            }
        }
        for (number, book) in CODE_BOOKS.iter().enumerate() {
            assert_eq!(book.codes.len(), book.base().pow(book.group as u32) as usize, "book {}", number + 1);
        }
    }

    #[test]
    fn group_code_index() {
        assert_eq!((CODE_BOOKS[0].index(&[0, 0, 0, 0]), CODE_BOOKS[0].index(&[1, 1, 1, 1])), (40, 80));
        assert_eq!((CODE_BOOKS[2].index(&[0, 0, 0, 0]), CODE_BOOKS[2].index(&[-2, 0, 1, -1])), (0, 54 + 3 + 1));
        assert_eq!((CODE_BOOKS[4].index(&[0, 0]), CODE_BOOKS[4].index(&[-4, 4])), (40, 8));
        assert_eq!((CODE_BOOKS[6].index(&[7, -7]), CODE_BOOKS[8].index(&[12, 1])), (63, 12 * 13 + 1));
        assert_eq!(
            CODE_BOOKS[10].index(&[-5000, 3]),
            16 * 17 + 3,
            "what is past the last book's values counts as its escape"
        );
        assert_eq!(CODE_BOOKS[0].code(40), (0, 1), "four zeros take one bit");
        assert_eq!((escape_ones(16), escape_ones(31), escape_ones(32), escape_ones(8191)), (0, 0, 1, 8));
    }

    #[test]
    fn powers_by_octave_steps() {
        assert_eq!((power(0), power(16), power(-32), power(160)), (1.0, 2.0, 0.25, 1024.0));
        assert!((power(8) - core::f32::consts::SQRT_2).abs() < 1.0e-6);
        assert!((power(-4) - 0.840_896_4).abs() < 1.0e-6, "a quarter of an octave down");
        assert!((power(15) * power(1) - 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn powers_bit_exact() {
        let mut power = 1.0f64;
        let iterated: [f32; OCTAVE_STEPS as usize] = core::array::from_fn(|_| {
            let value = power;
            power *= OCTAVE_ROOT;
            value as f32
        });
        assert_eq!(POWERS.map(f32::to_bits), iterated.map(f32::to_bits), "the table as an iterator makes it");
    }

    #[test]
    fn seconds_less_lead() {
        assert_eq!(
            [seconds(0, DELAY, RATE), seconds(1, DELAY, RATE), seconds(47, DELAY, RATE), seconds(48, DELAY, RATE)],
            [0, 0, 1, 2],
            "a recording's seconds, its encoder's lead taken off"
        );
        assert_eq!(
            (seconds(100, 1024, 48_000), seconds(47, 0, RATE), seconds(1, 0, 0)),
            (3, 2, 1024),
            "100 frames less the lead are 2.112 s, 47 without it 1.003 s, and a rate of none counts as one a second"
        );
    }

    #[test]
    fn track_and_cut_seconds() {
        use crate::codec::mp4::Sample;
        use crate::codec::mp4::Sound;
        let sound =
            Sound { rate: RATE, frame: FRAME as u32, channels: 1, configuration: CONFIGURATION.to_vec(), lead: 1024 };
        let packets = vec![Sample { at: 0, bytes: 10, time: 0, is_key: true }; 100];
        let mut track = Soundtrack { sound, packets, heard: None };
        assert_eq!(track_seconds(&track), 3, "100 frames less the lead are 2.112 s, a second begun counted whole");
        assert_eq!((cut_seconds(&track, 0, 47), cut_seconds(&track, 1, 47)), (1, 2));
        assert_eq!(track_samples(&track), 100 * 1024 - 1024);
        track.heard = Some(2 * 48_000);
        assert_eq!((track_samples(&track), track_seconds(&track)), (96_000, 2), "no more than the edit lets be heard");
        track.heard = Some(1_000_000);
        assert_eq!(track_samples(&track), 100 * 1024 - 1024, "and no more than the packets hold");
    }

    // The bits of a configuration from its fields: the object type, the rate's index and where that is 15 the rate, the channel configuration, the three flags and what follows.
    fn configuration(
        object: u32,
        index: u32,
        rate: Option<u32>,
        channels: u32,
        flags: u32,
        more: &[(u32, u32)],
    ) -> Vec<u8> {
        let mut bits = BitWriter::default();
        bits.put(object, OBJECT_BITS);
        bits.put(index, RATE_INDEX_BITS);
        if let Some(rate) = rate {
            bits.put(rate, EXPLICIT_RATE_BITS);
        }
        bits.put(channels, CHANNEL_CONFIG_BITS);
        bits.put(flags, FLAG_BITS);
        more.iter().for_each(|&(value, count)| bits.put(value, count));
        bits.finish()
    }

    #[test]
    fn configuration_read() {
        assert_eq!(Config::read(&CONFIGURATION), Ok(Config::OURS));
        assert_eq!(Config::bytes(LOW_COMPLEXITY, 3, 1), CONFIGURATION, "ours, written");
        assert_eq!((Config::OURS.rate(), Config::OURS.channels()), (RATE, 1));
        let stereo = Config::read(&[0x12, 0x10]).unwrap();
        assert_eq!((stereo.rate(), stereo.channels()), (44_100, 2));
        for (index, rate) in RATES.iter().enumerate() {
            let config = Config::read(&configuration(2, index as u32, None, 1, 0, &[])).unwrap();
            assert_eq!(config.rate(), *rate, "index {index}");
        }
        let explicit = configuration(2, EXPLICIT_RATE, Some(22_050), 2, 0, &[]);
        assert_eq!(Config::read(&explicit).map(Config::rate), Ok(22_050), "a rate that has an index, told in full");
        let backward = [0x12, 0x10, 0x56, 0xE5, 0x00];
        assert_eq!(
            Config::read(&backward),
            Ok(stereo),
            "as FFmpeg writes it: an extension that says there is no replication"
        );
        let refused = [
            (vec![0x12, 0x10, 0x56, 0xE5, 0x80], "spectral band replication"),
            (configuration(5, 3, None, 1, 0, &[]), "an object type that is not low complexity"),
            (configuration(1, 3, None, 1, 0, &[]), "an object type that is not low complexity"),
            (configuration(2, EXPLICIT_RATE, Some(44_000), 1, 0, &[]), "a sampling frequency without an index"),
            (configuration(2, 3, None, 0, 0, &[]), "a channel layout of the stream's own"),
            (configuration(2, 3, None, 6, 0, &[]), "more than two channels"),
            (configuration(2, 3, None, 1, 0b100, &[]), "frames of 960 samples"),
            (configuration(2, 3, None, 1, 0b010, &[]), "a core coder"),
            (configuration(2, 3, None, 1, 0b001, &[]), "an extended configuration"),
        ];
        for (bytes, what) in refused {
            assert_eq!(Config::read(&bytes), Err(Flaw::Unsupported(what)), "{bytes:x?}");
        }
        assert_eq!(Config::read(&configuration(2, 13, None, 1, 0, &[])), Err(Flaw::Broken), "an index kept back");
        assert_eq!(Config::read(&[0x11]), Err(Flaw::Short));
        assert_eq!(Config::read(&[]), Err(Flaw::Short));
    }

    #[test]
    fn band_tables_cover_the_window() {
        use tables::{LONG_BANDS, SHORT_BANDS, TNS_LONG_BANDS, TNS_SHORT_BANDS};
        assert_eq!(LONG_BANDS.map(|bands| bands.len() - 1), [41, 41, 47, 49, 49, 51, 47, 47, 43, 43, 43, 40, 40]);
        assert_eq!(SHORT_BANDS.map(|bands| bands.len() - 1), [12, 12, 12, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15]);
        for (tables, end) in [(LONG_BANDS, FRAME), (SHORT_BANDS, FRAME / transform::WINDOWS)] {
            assert_eq!(tables.len(), RATES.len());
            for bands in tables {
                assert_eq!((bands[0], usize::from(bands[bands.len() - 1])), (0, end));
                assert!(bands.windows(2).all(|pair| pair[0] < pair[1] && (pair[1] - pair[0]) % 4 == 0), "{bands:?}");
            }
        }
        for (limits, tables) in [(TNS_LONG_BANDS, LONG_BANDS), (TNS_SHORT_BANDS, SHORT_BANDS)] {
            assert!(limits.iter().zip(tables).all(|(&limit, bands)| usize::from(limit) < bands.len()));
        }
        assert_eq!(tables::BANDS.len() - 1, BANDS, "the encoder's bands are those of 48 kHz");
        assert!(core::ptr::eq(LONG_BANDS[3], &tables::BANDS[..]) && core::ptr::eq(LONG_BANDS[4], &tables::BANDS[..]));
    }

    #[test]
    fn reader_peeks_skips_and_aligns() {
        let bytes = [0b1010_1100, 0xAB, 0xCD, 0xEF];
        let mut reader = Reader(BitReader::new(&bytes));
        assert_eq!((reader.peek(4), reader.take(3)), (Ok(0b1010), Ok(0b101)));
        reader.align().unwrap();
        assert_eq!(reader.take(8), Ok(0xAB), "the rest of the byte is passed over");
        reader.align().unwrap();
        assert_eq!(reader.peek(8), Ok(0xCD), "and a reader at a byte's start stays");
        assert_eq!(reader.skip(24), Err(Flaw::Short), "more than is left, and nothing is passed over");
        assert_eq!(reader.skip(16), Ok(()));
        assert_eq!((reader.bit(), reader.peek(1)), (Err(Flaw::Short), Err(Flaw::Short)));
    }
}
