use super::decode::superframe_sizes;
use super::*;
use crate::base::checksum::crc32;
use crate::base::math::xorshift64;
use crate::codec::Flaw;
use crate::codec::ivf;
use crate::codec::picture::samples::i420;
use alloc::vec::Vec;

macro_rules! asset {
    ($name:literal) => {
        include_bytes!(concat!("../../../assets/video/vp9/", $name))
    };
}

// A key frame's first byte in profile 0: frame marker 2, profile 0, a frame to decode, a key frame, shown.
const KEY_BYTE: u8 = 0x82;
// The sync code of key and intra-only frames (VP9 7.2.1).
const SYNC_CODE: [u8; 3] = [0x49, 0x83, 0x42];
// The bits of a profile 0 key frame's header before its color space, and before its width less one (VP9 6.2).
const COLOR_SPACE_BIT: usize = 32;
const WIDTH_BIT: usize = 36;
// A frame of one byte that shows the frame in reference slot 0 again: marker 2, profile 0, show_existing_frame.
const SHOW_SLOT_0: u8 = 0x88;

// Streams encoded with libvpx 1.17.0, each frame shown as libvpx's vpxdec decodes it.
const ALTREF: &[u8] = asset!("altref.ivf");
const ALTREF_CRCS: [u32; 16] = [
    0x2EB1D4CB, 0x1B8BEC75, 0xF327F3C4, 0xED714CA1, 0x3CE59296, 0x17FADE8D, 0xD531BF79, 0x57AF29CF, 0xB5F2655D,
    0x3C348F9B, 0xE3612451, 0xD4CA3468, 0xB6E66162, 0xB20E42BC, 0x8D38D6E8, 0x25521F87,
];
const LOSSLESS: &[u8] = asset!("lossless.ivf");
const LOSSLESS_CRCS: [u32; 3] = [0x5BC938A0, 0x9E291E8F, 0xD86FD6A4];
const RESILIENT: &[u8] = asset!("resilient.ivf");
const RESILIENT_CRCS: [u32; 6] = [0xBF8F71F9, 0x57E7F0C9, 0x7A58EFB9, 0xEA3F2584, 0x5CF14DFF, 0x9C579482];
const RESIZE: &[u8] = asset!("resize.ivf");
const RESIZE_CRCS: [u32; 12] = [
    0xE11B9E87, 0x1ACF7A50, 0x19D11EF9, 0xEDCF0FA2, 0x6ECE8424, 0xA653F9F0, 0x8D6F3E0E, 0x9B9A3873, 0x0B72F8D4,
    0x12A75726, 0x871AB0F5, 0xCB02C4F8,
];
const SIZE_1X1: &[u8] = asset!("size-1x1.ivf");
const SIZE_1X1_CRCS: [u32; 3] = [0x2A45EF32; 3];
const SIZE_2X2: &[u8] = asset!("size-2x2.ivf");
const SIZE_2X2_CRCS: [u32; 4] = [0x3F5A9C75; 4];
const SIZE_33X17: &[u8] = asset!("size-33x17.ivf");
const SIZE_33X17_CRCS: [u32; 6] = [0x0F7FE820, 0xF1880285, 0x7FAA1789, 0x69A23FA8, 0x11C2C589, 0xFB5A9857];
// SVC's pictures by the spatial layers decoded: 80x44, then 160x88, then 320x176.
const SVC_CRCS: [[u32; 12]; 3] = [
    [
        0x9913C419, 0x65B67A07, 0x7C8BF89F, 0xEB582B14, 0x6D9A41A0, 0x07389A2C, 0xC133DF2A, 0x37ED1149, 0x463EDF7C,
        0x807D7AAF, 0x873C00AD, 0xB9E0E76F,
    ],
    [
        0xE24168FB, 0xEF33547E, 0xF5A2787F, 0x18B2AD7D, 0x22D4CD3D, 0xAA7E35EF, 0x64AE8A47, 0xE86C5D9A, 0xE92AEBB8,
        0x1FD0F412, 0xE6BEA4B1, 0xCF614A7F,
    ],
    [
        0x9915974E, 0x462B06C1, 0x51D29CAF, 0xD02E4F6C, 0xD988EAE7, 0x69CB8EA4, 0xFA4125F0, 0x85EFDDE8, 0xEB2CEC83,
        0xE7FDF47D, 0xFABDEE3B, 0x7116D857,
    ],
];
const TILES: &[u8] = asset!("tiles.ivf");
const TILES_CRCS: [u32; 2] = [0x97300ACE, 0xE0AE8190];

// The frames of a superframe by its index.
fn layers(packet: &[u8]) -> Vec<&[u8]> {
    let sizes = superframe_sizes(packet).unwrap().unwrap_or_else(|| alloc::vec![packet.len()]);
    let mut start = 0;
    sizes
        .into_iter()
        .map(|size| {
            start += size;
            &packet[start - size..start]
        })
        .collect()
}

// Frames under a superframe index of 4 byte sizes (VP9 B.3).
fn with_index(frames: &[&[u8]]) -> Vec<u8> {
    let marker = 0xC0 | 3 << 3 | (frames.len() as u8 - 1);
    let mut packet: Vec<u8> = frames.concat();
    packet.push(marker);
    for frame in frames {
        packet.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    }
    packet.push(marker);
    packet
}

// The CRC32 of the visible planes of each frame shown, or the flaw of the first data refused.
fn decode_all<'a>(decoder: &mut Decoder, packets: impl IntoIterator<Item = &'a [u8]>) -> Result<Vec<u32>, Flaw> {
    let mut crcs = Vec::new();
    for packet in packets {
        if let Some(picture) = decoder.decode(packet)? {
            crcs.push(crc32(&i420(&picture)));
        }
    }
    Ok(crcs)
}

fn decode_stream(file: &[u8]) -> Result<Vec<u32>, Flaw> {
    decode_all(&mut Decoder::default(), ivf::frames(file))
}

// Sets `count` bits of a header from bit `at`, highest first.
fn set_bits(frame: &mut [u8], at: usize, count: usize, value: u32) {
    for bit in 0..count {
        let (byte, shift) = ((at + bit) / 8, 7 - (at + bit) % 8);
        let is_set = value >> (count - 1 - bit) & 1 == 1;
        frame[byte] = frame[byte] & !(1 << shift) | u8::from(is_set) << shift;
    }
}

#[test]
fn streams_match_libvpx() {
    let streams: [(&str, &[u8], &[u32]); 9] = [
        ("hidden alternate reference frames in superframes, compound prediction", ALTREF, &ALTREF_CRCS),
        ("lossless: the Walsh-Hadamard transform", LOSSLESS, &LOSSLESS_CRCS),
        ("error resilient and frame parallel, no adaptation", RESILIENT, &RESILIENT_CRCS),
        ("320x176, 240x132 from scaled references, then 320x176 again, no key frame", RESIZE, &RESIZE_CRCS),
        ("1 by 1", SIZE_1X1, &SIZE_1X1_CRCS),
        ("2 by 2", SIZE_2X2, &SIZE_2X2_CRCS),
        ("33 by 17", SIZE_33X17, &SIZE_33X17_CRCS),
        ("three spatial layers in superframes, inter-layer prediction on every picture", SVC, &SVC_CRCS[2]),
        ("two tile columns and four tile rows", TILES, &TILES_CRCS),
    ];
    for (name, file, expected) in streams {
        assert_eq!(decode_stream(file).as_deref(), Ok(expected), "{name}: each frame as libvpx decodes it");
    }
}

#[test]
fn lower_layers_alone() {
    let packets = ivf::frames(SVC);
    for count in 1..=3 {
        let cut: Vec<Vec<u8>> = packets.iter().map(|packet| layers(packet)[..count].concat()).collect();
        assert_eq!(
            decode_all(&mut Decoder::default(), cut.iter().map(Vec::as_slice)).as_deref(),
            Ok(&SVC_CRCS[count - 1][..]),
            "the first {count} layers back to back, as a receiver joins a picture's layers: as libvpx decodes layer {}",
            count - 1
        );
        let indexed: Vec<Vec<u8>> = packets.iter().map(|packet| with_index(&layers(packet)[..count])).collect();
        assert_eq!(
            decode_all(&mut Decoder::default(), indexed.iter().map(Vec::as_slice)).as_deref(),
            Ok(&SVC_CRCS[count - 1][..]),
            "the first {count} layers under an index of their own"
        );
    }
    let mut decoder = Decoder::default();
    for (picture_index, packet) in packets.iter().enumerate() {
        for (layer, frame) in layers(packet).into_iter().enumerate() {
            let picture = decoder.decode(frame).unwrap().unwrap();
            assert_eq!(
                crc32(&i420(&picture)),
                SVC_CRCS[layer][picture_index],
                "picture {picture_index}, layer {layer} decoded alone: every layer is shown"
            );
        }
    }
}

#[test]
fn existing_frame_shown_again() {
    let mut decoder = Decoder::default();
    assert_eq!(decoder.decode(&[SHOW_SLOT_0]).err(), Some(Flaw::Missing("reference frame")), "an empty slot");
    assert_eq!(decode_all(&mut decoder, ivf::frames(LOSSLESS)).as_deref(), Ok(&LOSSLESS_CRCS[..]), "the stream");
    let picture = decoder.decode(&[SHOW_SLOT_0]).unwrap().unwrap();
    assert_eq!(crc32(&i420(&picture)), LOSSLESS_CRCS[2], "slot 0 holds the last frame, shown again");
    let frames = ivf::frames(LOSSLESS);
    let mut joined = frames[0].to_vec();
    joined.extend_from_slice(&[0, 0, SHOW_SLOT_0]);
    let picture = decoder.decode(&joined).unwrap().unwrap();
    assert_eq!(crc32(&i420(&picture)), LOSSLESS_CRCS[0], "a key frame, its padding, then the slot it refreshed");
}

#[test]
fn frames_refused_by_name() {
    let mut decoder = Decoder::default();
    let profile = |low: u8, high: u8| [0x80 | low << 5 | high << 4, 0, 0, 0];
    let refusals: [(&[u8], Flaw, &str); 4] = [
        (&profile(1, 0), Flaw::Unsupported("profile 1, of 4:2:2, 4:4:0 or 4:4:4"), "profile 1"),
        (&profile(0, 1), Flaw::Unsupported("profile 2, of 10 or 12 bits"), "profile 2"),
        (&profile(1, 1), Flaw::Unsupported("profile 3, of 10 or 12 bits and 4:2:2, 4:4:0 or 4:4:4"), "profile 3"),
        (&[0x42, 0, 0, 0], Flaw::Invalid("frame marker"), "a frame marker of 1"),
    ];
    for (frame, flaw, name) in refusals {
        assert_eq!(decoder.decode(frame).err(), Some(flaw), "{name}");
    }
    let frames = ivf::frames(TILES);
    assert!(decoder.needs_key_frame(), "a new decoder waits for a key frame");
    assert_eq!(decoder.decode(frames[1]).err(), Some(Flaw::Missing("key frame")), "an inter frame first");
    let mut rgb = frames[0].to_vec();
    set_bits(&mut rgb, COLOR_SPACE_BIT, 3, 7);
    assert_eq!(decoder.decode(&rgb).err(), Some(Flaw::Unsupported("RGB, which is 4:4:4")), "RGB in profile 0");
    for (width, height, name) in [(2048, 1088, "8704 macroblocks"), (4097, 16, "257 macroblocks across")] {
        let mut large = frames[0].to_vec();
        set_bits(&mut large, WIDTH_BIT, 16, width - 1);
        set_bits(&mut large, WIDTH_BIT + 16, 16, height - 1);
        assert_eq!(
            decoder.decode(&large).err(),
            Some(Flaw::Unsupported("a picture of more than 8192 macroblocks or 256 across or down")),
            "{name}: refused before anything of its size is allocated"
        );
    }
    // Cut where the last byte cannot be taken for a superframe index's marker, 0xC0 to 0xDF.
    let end = (frames[0].len() / 2..).find(|&end| frames[0][end - 1] & 0xE0 != 0xC0).unwrap();
    assert_eq!(decoder.decode(&frames[0][..end]).err(), Some(Flaw::Short), "a key frame cut short");
    assert_eq!(decode_all(&mut decoder, frames.iter().copied()).as_deref(), Ok(&TILES_CRCS[..]), "the stream after");
    assert!(!decoder.needs_key_frame(), "synced");
    let mut broken = frames[1].to_vec();
    broken.push(0xC1);
    assert_eq!(decoder.decode(&broken).err(), Some(Flaw::Invalid("superframe index")), "an index's last marker alone");
    assert!(decoder.needs_key_frame(), "after an error the decoder waits for a key frame or an intra-only frame");
    assert_eq!(decoder.decode(frames[1]).err(), Some(Flaw::Missing("key frame")), "a good inter frame is refused");
    assert_eq!(decoder.decode(&[SHOW_SLOT_0]), Ok(None), "a slot shown while waiting shows nothing");
    let picture = decoder.decode(frames[0]).unwrap().unwrap();
    assert_eq!(crc32(&i420(&picture)), TILES_CRCS[0], "the key frame decodes as it did");
}

#[test]
fn damaged_frames_return() {
    let streams = [TILES, LOSSLESS, RESILIENT, ALTREF, SIZE_33X17, SIZE_2X2, SIZE_1X1, SVC, RESIZE].map(ivf::frames);
    let key = streams[0][0];
    let mut state = 0x5650_3920_5250u64;
    let mut below = |bound: usize| {
        state = xorshift64(state);
        (state % bound as u64) as usize
    };
    let mut decoder = Decoder::default();
    let mut calls = 0;
    for round in 0..3200 {
        let frames = &streams[round % streams.len()];
        match round % 4 {
            0 => {
                for frame in frames {
                    let mut damaged = frame.to_vec();
                    for _ in 0..1 + below(8) {
                        let at = below(damaged.len());
                        damaged[at] ^= 1 << below(8);
                    }
                    let _ = decoder.decode(&damaged);
                    calls += 1;
                }
            }
            1 => {
                for frame in frames {
                    let _ = decoder.decode(&frame[..below(frame.len())]);
                    calls += 1;
                }
            }
            2 => {
                // Frames joined without an index, and under an index whose sizes are damaged.
                for pair in frames.chunks(2) {
                    let joined = pair.concat();
                    let _ = decoder.decode(&joined);
                    let mut indexed = with_index(pair);
                    let at = indexed.len() - 2 - below(4);
                    indexed[at] ^= 1 << below(8);
                    let _ = decoder.decode(&indexed);
                    calls += 2;
                }
            }
            _ => {
                for _ in 0..frames.len() {
                    let mut bytes: Vec<u8> = (0..below(900)).map(|_| below(1 << u8::BITS) as u8).collect();
                    if bytes.len() >= 12 && below(2) == 0 {
                        // A key frame's first byte, sync code and a small size, so that the header is read past them.
                        bytes[0] = KEY_BYTE;
                        bytes[1..4].copy_from_slice(&SYNC_CODE);
                        set_bits(&mut bytes, WIDTH_BIT, 16, below(200) as u32);
                        set_bits(&mut bytes, WIDTH_BIT + 16, 16, below(200) as u32);
                    }
                    let _ = decoder.decode(&bytes);
                    calls += 1;
                }
            }
        }
        let picture = decoder.decode(key).unwrap().unwrap();
        assert_eq!(
            crc32(&i420(&picture)),
            TILES_CRCS[0],
            "round {round}: a good key frame after the damage decodes exactly"
        );
    }
    assert!(calls > 20_000, "{calls} damaged frames");
}
