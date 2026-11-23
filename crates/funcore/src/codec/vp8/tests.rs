use super::*;
use crate::base::checksum::crc32;
use crate::codec::Flaw;
use crate::codec::ivf;
use crate::codec::picture::samples::i420;
use alloc::vec::Vec;

macro_rules! asset {
    ($name:literal) => {
        include_bytes!(concat!("../../../assets/video/vp8/", $name))
    };
}

pub(crate) const CALL: &[u8] = asset!("call.ivf");
pub(crate) const CALL_CRCS: [u32; 20] = [
    0xF7ADF4EA, 0x6D8ACF8D, 0x9069409F, 0x4CB582AC, 0x4B42EF9F, 0x6C89C332, 0x4E0B7D41, 0x3D14D431, 0x17416058,
    0x4ED7249D, 0x25E79EF5, 0xF445E914, 0x1254F5E5, 0xA51B3A4A, 0xC0229574, 0xF374F612, 0x0A13B1AD, 0x4172B1DA,
    0xB5469D23, 0xDF72A930,
];
pub(crate) const ALTREF: &[u8] = asset!("altref.ivf");
pub(crate) const ALTREF_CRCS: [u32; 34] = [
    0x15A91E02, 0xE949AEE6, 0x60C0577F, 0x243D04EC, 0x68A3194F, 0xD6DAD3A8, 0x7E9ADCBD, 0xFE7D9A1C, 0xB23AE591,
    0xCE7DA578, 0x279ECB4A, 0x38BAD259, 0x60018C60, 0xCFC2937F, 0xC014AC5F, 0x8F502061, 0xC560F8DB, 0x9521B364,
    0x6D4E8C5A, 0x12C9511A, 0x0B1C73D3, 0xE3388F46, 0x3EA848E6, 0xEFBFCC73, 0x863CDEF4, 0xCD52E50D, 0xB5A0BFDD,
    0x59C5B505, 0xBE84E3B5, 0xFAA1DD9C, 0xEF9BD194, 0x833156FE, 0x07A2B63D, 0x2B4854CC,
];
pub(crate) const BILINEAR: &[u8] = asset!("bilinear.ivf");
pub(crate) const BILINEAR_CRCS: [u32; 16] = [
    0x11112A24, 0x89534E7D, 0x35C3D9E7, 0x63FB172B, 0x62E8CF0B, 0x27179181, 0xEDE10F6E, 0x61A41B7B, 0x34EC7956,
    0x44761934, 0x4D03AD22, 0x9913E647, 0x827E710B, 0x5A760059, 0x14F78511, 0x9936F973,
];
pub(crate) const VERSIONS: &[u8] = asset!("versions.ivf");
pub(crate) const VERSIONS_CRCS: [u32; 20] = [
    0xC70E2000, 0xFCCE300A, 0x81FDFCE4, 0x296C9FC0, 0x656494D4, 0x95538EFC, 0xC6BAFB50, 0x80B4D723, 0x31A18CE8,
    0xA834AE31, 0xACA60B8E, 0x36B18A6F, 0xA920DF44, 0xFBA81F01, 0xEFDCCBDD, 0x275CC321, 0x4A480E96, 0x221F6F67,
    0x431C4AE9, 0x93B5AEEF,
];

// The CRC32 of the visible planes of each frame shown, or the flaw of the first frame refused.
pub(crate) fn decode_frames<'a>(frames: impl IntoIterator<Item = &'a [u8]>) -> Result<Vec<u32>, Flaw> {
    let mut decoder = Decoder::default();
    let mut crcs = Vec::new();
    for frame in frames {
        if let Some(picture) = decoder.decode(frame)? {
            crcs.push(crc32(&i420(&picture)));
        }
    }
    Ok(crcs)
}

#[test]
fn streams_match_ffmpeg() {
    let streams: [(&str, &[u8], &[u32]); 4] = [
        ("a call: segments, eight partitions, 99 by 61", CALL, &CALL_CRCS),
        ("altref frames not shown, golden frames, sharpness, two partitions", ALTREF, &ALTREF_CRCS),
        ("version 1: bilinear, the simple filter, four partitions, 111 by 79", BILINEAR, &BILINEAR_CRCS),
        ("versions 2 and 3, whole pixel chroma, 80 by 48 then 71 by 39", VERSIONS, &VERSIONS_CRCS),
    ];
    for (name, file, expected) in streams {
        assert_eq!(
            decode_frames(ivf::frames(file)).as_deref(),
            Ok(expected),
            "{name}: each frame as ffmpeg decodes it"
        );
    }
}

#[test]
fn frames_refused_by_name() {
    let frames = ivf::frames(CALL);
    let mut decoder = Decoder::default();
    assert!(decoder.needs_key_frame(), "a new decoder has no reference");
    assert_eq!(decoder.decode(frames[1]), Err(Flaw::Missing("key frame")), "an interframe before any key frame");
    assert!(is_key_frame(frames[0]) && !is_key_frame(frames[1]), "told from the first bytes");
    assert!(decoder.decode(frames[0]).is_ok_and(|picture| picture.is_some()), "the key frame is shown");
    assert!(!decoder.needs_key_frame(), "the key frame is a reference");
    assert_eq!(decoder.decode(&frames[1][..2]), Err(Flaw::Short), "a frame tag cut short");
    assert!(decoder.needs_key_frame(), "after an error the decoder waits for a key frame");
    assert_eq!(decoder.decode(frames[2]), Err(Flaw::Missing("key frame")), "a good interframe is refused until then");
    let mut cut = frames[0].to_vec();
    cut.truncate(cut.len() / 2);
    assert_eq!(decoder.decode(&cut), Err(Flaw::Short), "a key frame cut in its tokens");
    let mut large = frames[0].to_vec();
    large[6..10].copy_from_slice(&[0x00, 0x18, 0x00, 0x18]);
    assert!(matches!(decoder.decode(&large), Err(Flaw::Unsupported(_))), "6144 by 6144 pixels");
    let mut experimental = frames[0].to_vec();
    experimental[0] |= 0x08;
    assert_eq!(
        decoder.decode(&experimental),
        Err(Flaw::Unsupported("a version beyond 3")),
        "the tag's experimental bit"
    );
    let picture = decoder.decode(frames[0]).unwrap().unwrap();
    assert_eq!(
        (picture.width, picture.height, picture.strides, crc32(&i420(&picture))),
        (99, 61, [112 + 64, 56 + 32, 56 + 32], CALL_CRCS[0]),
        "the next key frame decodes as it did; the planes' lines hold the macroblocks and a border on either side"
    );
    let shown = ivf::frames(ALTREF).iter().filter(|frame| frame[0] >> 4 & 1 == 0).count();
    assert_eq!(shown, 2, "the two altref frames are not shown");
}

// A generator of numbers that looks random and is the same on every run (SplitMix64).
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

#[test]
fn damaged_frames_return() {
    let streams = [CALL, BILINEAR, VERSIONS, ALTREF].map(ivf::frames);
    let key = streams[0][0];
    let mut noise = Noise(0x5650_3820_5250);
    let mut decoder = Decoder::default();
    let mut calls = 0;
    for round in 0..300 {
        let frames = &streams[round % streams.len()];
        match round % 3 {
            0 => {
                for frame in frames {
                    let mut damaged = frame.to_vec();
                    for _ in 0..1 + noise.below(8) {
                        let at = noise.below(damaged.len());
                        damaged[at] ^= 1 << noise.below(8);
                    }
                    let _ = decoder.decode(&damaged);
                    calls += 1;
                }
            }
            1 => {
                for frame in frames {
                    let _ = decoder.decode(&frame[..noise.below(frame.len())]);
                    calls += 1;
                }
            }
            _ => {
                for _ in 0..frames.len() {
                    let mut bytes: Vec<u8> = (0..noise.below(600)).map(|_| noise.next() as u8).collect();
                    if bytes.len() >= 10 && noise.below(2) == 0 {
                        // A key frame's tag, start code and a small size, so that the header is read past them.
                        let tag = (noise.below(bytes.len()) as u32) << 5 | 1 << 4;
                        bytes[..3].copy_from_slice(&tag.to_le_bytes()[..3]);
                        bytes[3..6].copy_from_slice(&[0x9D, 0x01, 0x2A]);
                        bytes[6] = 1 + noise.below(200) as u8;
                        bytes[8] = 1 + noise.below(200) as u8;
                    }
                    let _ = decoder.decode(&bytes);
                    calls += 1;
                }
            }
        }
        let picture = decoder.decode(key).unwrap().unwrap();
        assert_eq!(
            crc32(&i420(&picture)),
            CALL_CRCS[0],
            "round {round}: a good key frame after the damage decodes exactly"
        );
    }
    assert!(calls > 5000, "{calls} damaged frames");
}
