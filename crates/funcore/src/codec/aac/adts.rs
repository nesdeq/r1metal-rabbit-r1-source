// AAC in the stream form of files that hold nothing else: every packet behind a header of its own (ISO/IEC 14496-3 Annex 1.A).

use super::tables::RATES;
use super::{Config, FRAME};
use crate::base::bits::BitReader;
use crate::codec::Flaw;
use crate::codec::mp4::{Sample, Sound, Soundtrack};
use alloc::vec::Vec;

const SYNC: u32 = 0xFFF;
const SYNC_BITS: u32 = 12;
const LAYER: u32 = 0;
const LAYER_BITS: u32 = 2;
const PROFILE_BITS: u32 = 2;
const RATE_BITS: u32 = 4;
const CHANNEL_BITS: u32 = 3;
const HEADER_BYTES: usize = 7;
const HEADER_BYTES_WITH_CHECK: usize = 9;
const LENGTH_BITS: u32 = 13;
// The bits of the originality, home and copyright flags between the channels and the length, and of the buffer fullness between the length and the count of raw data blocks.
const COPY_BITS: u32 = 4;
const FULLNESS_BITS: u32 = 11;
const BLOCK_BITS: u32 = 2;
// What an ID3 tag in front of a stream begins with, the bytes of its header, the flag that says a footer of as many bytes follows its content, and the bits of the content's size in each of the four bytes that tell it.
const TAG: &[u8; 3] = b"ID3";
const TAG_HEADER_BYTES: usize = 10;
const TAG_FLAGS_AT: usize = 5;
const TAG_SIZE_AT: usize = 6;
const TAG_FOOTER: u8 = 0x10;
const TAG_SIZE_BITS: u32 = 7;

struct Header {
    // The object type, the rate's index and the channel configuration: what every header of a stream tells alike.
    kind: (u32, usize, u32),
    size: usize,
    length: usize,
    // Raw data blocks behind the header, less one.
    blocks: u32,
}

fn header(bytes: &[u8]) -> Option<Header> {
    let mut reading = BitReader::new(bytes.get(..HEADER_BYTES)?);
    let sync = reading.take(SYNC_BITS)?;
    reading.bit()?;
    let layer = reading.take(LAYER_BITS)?;
    let is_protected = reading.bit()? == 0;
    let profile = reading.take(PROFILE_BITS)?;
    let rate = reading.take(RATE_BITS)? as usize;
    reading.bit()?;
    let channels = reading.take(CHANNEL_BITS)?;
    reading.take(COPY_BITS)?;
    let length = reading.take(LENGTH_BITS)? as usize;
    reading.take(FULLNESS_BITS)?;
    let blocks = reading.take(BLOCK_BITS)?;
    let size = if is_protected { HEADER_BYTES_WITH_CHECK } else { HEADER_BYTES };
    (sync == SYNC && layer == LAYER && length >= size).then_some(Header {
        kind: (profile + 1, rate, channels),
        size,
        length,
        blocks,
    })
}

fn tag(bytes: &[u8]) -> usize {
    let Some(head) = bytes.get(..TAG_HEADER_BYTES).filter(|head| head.starts_with(TAG)) else {
        return 0;
    };
    let content = head[TAG_SIZE_AT..]
        .iter()
        .fold(0usize, |size, &byte| size << TAG_SIZE_BITS | usize::from(byte) & ((1 << TAG_SIZE_BITS) - 1));
    let footer = if head[TAG_FLAGS_AT] & TAG_FOOTER != 0 { TAG_HEADER_BYTES } else { 0 };
    TAG_HEADER_BYTES + content + footer
}

// Whether a file is a stream of this form: it begins with a header, or with a tag that stands before one.
pub(super) fn begins(file: &[u8]) -> bool {
    header(file).is_some() || file.starts_with(TAG)
}

// The sound of a file held whole that is a stream of this form; the stream ends at the first header that is none, tells another kind of stream or has several raw data blocks behind it, or at a packet the file ends in.
pub(super) fn read(file: &[u8]) -> Result<Soundtrack, Flaw> {
    let mut at = tag(file);
    let first = file.get(at..).and_then(header).ok_or(Flaw::Missing("a header of a stream of AAC"))?;
    let (object, rate, channels) = first.kind;
    let rate = RATES.get(rate).ok_or(Flaw::Invalid("a sampling frequency the standard keeps back"))?;
    if first.blocks != 0 {
        return Err(Flaw::Unsupported("several raw data blocks behind one header"));
    }
    let mut packets = Vec::new();
    let is_next = |head: &Header| head.kind == first.kind && head.blocks == 0;
    while let Some(head) = file.get(at..).and_then(header).filter(is_next) {
        if at + head.length > file.len() {
            break;
        }
        let time = (packets.len() * FRAME) as u64;
        packets.push(Sample {
            at: (at + head.size) as u64,
            bytes: (head.length - head.size) as u32,
            time,
            is_key: true,
        });
        at += head.length;
    }
    let configuration = Config::bytes(object, first.kind.1 as u32, channels);
    let sound = Sound { rate: *rate, frame: FRAME as u32, channels: channels as u16, configuration, lead: 0 };
    Ok(Soundtrack { sound, packets, heard: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::bits::BitWriter;
    use alloc::vec;

    // A header of a packet of that many bytes behind it: object type 2, rate index 4, two channels.
    fn headed(packet: &[u8], is_protected: bool, blocks: u32) -> Vec<u8> {
        let size = if is_protected { HEADER_BYTES_WITH_CHECK } else { HEADER_BYTES };
        let length = (packet.len() + size) as u32;
        let mut bits = BitWriter::default();
        for (value, count) in [
            (SYNC, SYNC_BITS),
            (0, 1),
            (LAYER, LAYER_BITS),
            (u32::from(!is_protected), 1),
            (1, PROFILE_BITS),
            (4, RATE_BITS),
            (0, 1),
            (2, CHANNEL_BITS),
            (0, COPY_BITS),
            (length, LENGTH_BITS),
            (0x7FF, FULLNESS_BITS),
            (blocks, BLOCK_BITS),
        ] {
            bits.put(value, count);
        }
        let mut bytes = bits.finish();
        if is_protected {
            bytes.extend_from_slice(&[0xAB, 0xCD]);
        }
        bytes.extend_from_slice(packet);
        bytes
    }

    fn stream(packets: &[&[u8]], is_protected: bool) -> Vec<u8> {
        packets.iter().flat_map(|packet| headed(packet, is_protected, 0)).collect()
    }

    #[test]
    fn packets_where_headers_say() {
        let file = stream(&[&[1; 10], &[2; 300], &[3; 5]], false);
        let track = read(&file).unwrap();
        assert_eq!(
            track.sound,
            Sound { rate: 44_100, frame: 1024, channels: 2, configuration: vec![0x12, 0x10], lead: 0 },
            "low complexity, 44.1 kHz, two channels"
        );
        let places: Vec<(u64, u32, u64)> =
            track.packets.iter().map(|packet| (packet.at, packet.bytes, packet.time)).collect();
        assert_eq!(places, [(7, 10, 0), (24, 300, 1024), (331, 5, 2048)]);
        assert!(track.packets.iter().all(|packet| packet.is_key && packet.fits(file.len() as u64)));
        assert_eq!(track.heard, None, "a stream has no edit list");
        assert_eq!(&file[7..17], [1; 10]);
    }

    #[test]
    fn checksum_and_tag() {
        let mut file = vec![b'I', b'D', b'3', 4, 0, 0x10, 0, 0, 0x01, 0x05];
        file.extend_from_slice(&[0x55; 128 + 5 + 10]);
        file.extend_from_slice(&stream(&[&[7; 20], &[8; 4]], true));
        assert!(begins(&file));
        let places: Vec<(u64, u32)> =
            read(&file).unwrap().packets.iter().map(|packet| (packet.at, packet.bytes)).collect();
        assert_eq!(places, [(10 + 128 + 5 + 10 + 9, 20), (10 + 143 + 29 + 9, 4)]);
    }

    #[test]
    fn stream_ends() {
        let mut file = stream(&[&[1; 10], &[2; 10]], false);
        let other = headed(&[3; 10], false, 0);
        let mut changed = other.clone();
        changed[2] ^= 0x04;
        let several = headed(&[3; 10], false, 1);
        for behind in [&[0u8; 40][..], &changed[..], &several[..], b"TAG some trailing bytes of another kind"] {
            let mut ended = file.clone();
            ended.extend_from_slice(behind);
            assert_eq!(read(&ended).unwrap().packets.len(), 2);
        }
        file.extend_from_slice(&other[..other.len() - 1]);
        assert_eq!(read(&file).unwrap().packets.len(), 2, "a packet cut short by the file's end");
    }

    #[test]
    fn refused_streams() {
        assert!(!begins(b"\0\0\0\x20ftypM4A "));
        assert_eq!(read(b"\0\0\0\x20ftypM4A ").err(), Some(Flaw::Missing("a header of a stream of AAC")));
        assert_eq!(read(&[]).err(), Some(Flaw::Missing("a header of a stream of AAC")));
        let blocks = headed(&[1; 4], false, 1);
        assert_eq!(read(&blocks).err(), Some(Flaw::Unsupported("several raw data blocks behind one header")));
        let mut reserved = headed(&[1; 4], false, 0);
        reserved[2] |= 0x3C;
        assert_eq!(read(&reserved).err(), Some(Flaw::Invalid("a sampling frequency the standard keeps back")));
        let mut layered = headed(&[1; 4], false, 0);
        layered[1] |= 0x02;
        assert!(!begins(&layered), "a layer that is none of ours");
    }
}
