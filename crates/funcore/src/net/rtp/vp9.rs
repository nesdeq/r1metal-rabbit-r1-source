use super::vp8::Picture;
use crate::base::bits::BitReader;
use alloc::vec::Vec;

// RFC 9628, 4.2: the first byte's I, P, L, F, B, E, V and Z bits; TID, U, SID and D; P_DIFF and N.
const PICTURE: u32 = 0x80;
const INTER_PICTURE: u32 = 0x40;
const LAYERS: u32 = 0x20;
const FLEXIBLE: u32 = 0x10;
const BEGIN: u32 = 0x08;
const END: u32 = 0x04;
const STRUCTURE: u32 = 0x02;
const NOT_UPPER_REFERENCE: u32 = 0x01;
const TID_BITS: u32 = 3;
const SID_BITS: u32 = 3;
const DIFF_BITS: u32 = 7;
// 4.2.1: N_S, Y, G and three bits reserved; a size of 16 bits each way; N_G; TID, U, R and two bits reserved.
const SPATIAL_COUNT_BITS: u32 = 3;
const RESERVED_SS_BITS: u32 = 3;
const SIZE_BITS: u32 = 16;
const GROUP_COUNT_BITS: u32 = 8;
const GROUP_DIFFS_BITS: u32 = 2;
const RESERVED_GROUP_BITS: u32 = 2;
// libwebrtc's bounds on what it reads (video_rtp_depacketizer_vp9.cc): spatial ids under `kMaxSpatialLayers` (video_codec_constants.h) and three references at most (vp9_globals.h, `kMaxVp9RefPics`).
const MOST_SPATIAL: u8 = 5;
const MOST_REFERENCES: usize = 3;

// What a VP9 payload descriptor tells (RFC 9628, 4.2).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Descriptor {
    pub(super) picture: Option<Picture>,
    pub(super) is_inter_picture: bool,
    pub(super) layer: Option<Layer>,
    pub(super) is_flexible: bool,
    pub(super) is_begin: bool,
    pub(super) is_end: bool,
    is_not_upper_reference: bool,
    // P_DIFF of each picture referred to, in flexible mode.
    pub(super) diffs: Vec<u8>,
    pub(super) structure: Option<Structure>,
}

// The layer indices of a layer frame, with TL0PICIDX in non-flexible mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Layer {
    temporal: u8,
    is_switch_up: bool,
    pub(super) spatial: u8,
    pub(super) is_inter_layer: bool,
    tl0: Option<u8>,
}

// The scalability structure (4.2.1): the spatial layers, their sizes where present, and the pictures of a picture group.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Structure {
    spatial_layers: u8,
    sizes: Vec<(u16, u16)>,
    group: Vec<GroupPicture>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct GroupPicture {
    temporal: u8,
    is_switch_up: bool,
    diffs: Vec<u8>,
}

impl Descriptor {
    // The descriptor at a payload's start and the VP9 data after it; none where it is cut short, references come without a picture id or past three, a spatial id lies past libwebrtc's, or no data follows (video_rtp_depacketizer_vp9.cc).
    pub(crate) fn read(payload: &[u8]) -> Option<(Descriptor, &[u8])> {
        let mut reader = BitReader::new(payload);
        let first = reader.take(u8::BITS)?;
        let mut descriptor = Descriptor {
            is_inter_picture: first & INTER_PICTURE != 0,
            is_flexible: first & FLEXIBLE != 0,
            is_begin: first & BEGIN != 0,
            is_end: first & END != 0,
            is_not_upper_reference: first & NOT_UPPER_REFERENCE != 0,
            ..Descriptor::default()
        };
        if first & PICTURE != 0 {
            let is_long = reader.flag()?;
            let id = reader.take(Picture { id: 0, is_long }.bits())? as u16;
            descriptor.picture = Some(Picture { id, is_long });
        }
        if first & LAYERS != 0 {
            let temporal = reader.take(TID_BITS)? as u8;
            let is_switch_up = reader.flag()?;
            let spatial = reader.take(SID_BITS)? as u8;
            let is_inter_layer = reader.flag()?;
            let tl0 = if descriptor.is_flexible { None } else { Some(reader.take(u8::BITS)? as u8) };
            if spatial >= MOST_SPATIAL {
                return None;
            }
            descriptor.layer = Some(Layer { temporal, is_switch_up, spatial, is_inter_layer, tl0 });
        }
        if descriptor.is_inter_picture && descriptor.is_flexible {
            descriptor.picture?;
            loop {
                if descriptor.diffs.len() == MOST_REFERENCES {
                    return None;
                }
                descriptor.diffs.push(reader.take(DIFF_BITS)? as u8);
                if !reader.flag()? {
                    break;
                }
            }
        }
        if first & STRUCTURE != 0 {
            descriptor.structure = Some(Structure::read(&mut reader)?);
        }
        let rest = payload.get(payload.len() - reader.remaining() / u8::BITS as usize..)?;
        (!rest.is_empty()).then_some((descriptor, rest))
    }

    // Whether the layer frame stands without inter-picture and inter-layer prediction, as libwebrtc tells a key frame (video_rtp_depacketizer_vp9.cc).
    pub(super) fn is_key(&self) -> bool {
        !self.is_inter_picture && !self.layer.is_some_and(|layer| layer.is_inter_layer)
    }
}

impl Structure {
    fn read(reader: &mut BitReader) -> Option<Structure> {
        let spatial_layers = reader.take(SPATIAL_COUNT_BITS)? as u8 + 1;
        let has_sizes = reader.flag()?;
        let has_group = reader.flag()?;
        reader.skip(RESERVED_SS_BITS as usize)?;
        let mut structure = Structure { spatial_layers, ..Structure::default() };
        if has_sizes {
            for _ in 0..spatial_layers {
                let width = reader.take(SIZE_BITS)? as u16;
                structure.sizes.push((width, reader.take(SIZE_BITS)? as u16));
            }
        }
        let count = if has_group { reader.take(GROUP_COUNT_BITS)? } else { 0 };
        for _ in 0..count {
            let temporal = reader.take(TID_BITS)? as u8;
            let is_switch_up = reader.flag()?;
            let references = reader.take(GROUP_DIFFS_BITS)?;
            reader.skip(RESERVED_GROUP_BITS as usize)?;
            let diffs = (0..references).map(|_| Some(reader.take(u8::BITS)? as u8)).collect::<Option<Vec<_>>>()?;
            structure.group.push(GroupPicture { temporal, is_switch_up, diffs });
        }
        Some(structure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn read(payload: &[u8]) -> Option<(Descriptor, usize)> {
        Descriptor::read(payload).map(|(descriptor, rest)| (descriptor, payload.len() - rest.len()))
    }

    // libwebrtc's video_rtp_depacketizer_vp9_unittest.cc, case by case.
    #[test]
    fn libwebrtc_depacketizer_cases() {
        let (basic, at) = read(&[0x0C, 0, 0, 0]).unwrap();
        assert_eq!(
            (basic.is_begin, basic.is_end, basic.picture, basic.layer, at),
            (true, true, None, None, 1),
            "ParseBasicHeader"
        );
        let mut packet = [0u8; 10];
        packet[..2].copy_from_slice(&[0x80, 0x7F]);
        assert_eq!(
            read(&packet).map(|(read, at)| (read.picture, at)),
            Some((Some(Picture { id: 0x7F, is_long: false }), 2)),
            "ParseOneBytePictureId"
        );
        packet[..3].copy_from_slice(&[0x80, 0xFF, 0xFF]);
        assert_eq!(
            read(&packet).map(|(read, at)| (read.picture, at)),
            Some((Some(Picture { id: 0x7FFF, is_long: true }), 3)),
            "ParseTwoBytePictureId"
        );
        let mut packet = [0u8; 13];
        packet[..3].copy_from_slice(&[0x20, 2 << 5 | 1 << 4 | 1 << 1 | 1, 17]);
        let layer = Layer { temporal: 2, is_switch_up: true, spatial: 1, is_inter_layer: true, tl0: Some(17) };
        assert_eq!(read(&packet).map(|(read, at)| (read.layer, at)), Some((Some(layer), 3)), "non-flexible mode");
        packet[..2].copy_from_slice(&[0x38, 2 << 5 | 1 << 4]);
        let layer = Layer { temporal: 2, is_switch_up: true, ..Layer::default() };
        let (flexible, at) = read(&packet).unwrap();
        assert_eq!(
            (flexible.layer, flexible.is_flexible, flexible.is_begin, at),
            (Some(layer), true, true, 2),
            "flexible"
        );
        packet[..6].copy_from_slice(&[0xD8, 0x80, 17, 17 << 1 | 1, 18 << 1 | 1, 127 << 1]);
        let (references, at) = read(&packet).unwrap();
        assert_eq!(
            (references.picture, references.diffs.clone(), at),
            (Some(Picture { id: 17, is_long: true }), vec![17, 18, 127], 6),
            "ParseRefIdx"
        );
        packet[..2].copy_from_slice(&[0x58, 3 << 1]);
        assert_eq!(read(&packet), None, "ParseRefIdxFailsWithNoPictureId");
        packet[..6].copy_from_slice(&[0xD8, 0x7F, 3 << 1 | 1, 3 << 1 | 1, 3 << 1 | 1, 3 << 1]);
        assert_eq!(read(&packet), None, "ParseRefIdxFailsWithTooManyRefPics");
        let mut packet = [0u8; 23];
        packet[..6].copy_from_slice(&[0x0A, 1 << 5 | 1 << 3, 2, 1 << 4, 2 << 5 | 1 << 2, 33]);
        let (structured, at) = read(&packet).unwrap();
        let group = vec![
            GroupPicture { temporal: 0, is_switch_up: true, diffs: vec![] },
            GroupPicture { temporal: 2, is_switch_up: false, diffs: vec![33] },
        ];
        assert_eq!(
            (structured.structure, at),
            (Some(Structure { spatial_layers: 2, sizes: vec![], group }), 6),
            "ParseSsData"
        );
        let (key, _) = read(&[0x08, 0]).unwrap();
        assert!(key.is_key() && key.is_begin && !key.is_end, "ParseFirstPacketInKeyFrame");
        let (delta, _) = read(&[0x44, 0]).unwrap();
        assert!(!delta.is_key() && !delta.is_begin && delta.is_end, "ParseLastPacketInDeltaFrame");
        let mut packet = [0u8; 20];
        packet[..10].copy_from_slice(&[0x0A, 1 << 5 | 1 << 4, 0x02, 0x80, 0x01, 0x68, 0x05, 0x00, 0x02, 0xD0]);
        let (sized, _) = read(&packet).unwrap();
        assert_eq!(sized.structure.unwrap().sizes, [(640, 360), (1280, 720)], "ParseResolution");
        assert_eq!(read(&[]), None, "ParseFailsForNoPayloadLength");
        assert_eq!(read(&[0]), None, "ParseFailsForTooShortBufferToFitPayload");
        assert!(!read(&[0x08, 0]).unwrap().0.is_not_upper_reference, "ParseNonRefForInterLayerPred");
        assert!(read(&[0x05, 0]).unwrap().0.is_not_upper_reference);
        let (inter_layer, _) = read(&[0x20, 0x01, 0, 0]).unwrap();
        assert!(!inter_layer.is_key(), "InterLayerPredOnlyFrameMerkedAsDelta");
        assert_eq!(read(&[0x20, 5 << 1, 0, 0]), None, "spatial id 5, past libwebrtc's");
    }

    // Written by hand from RFC 9628, 4.2 and 4.2.1: the first two layer frames of a key picture in flexible mode, three spatial layers of 160x120, 320x240 and 640x480 as L3T3_KEY has them.
    #[test]
    fn key_picture_of_three_layers() {
        let first = [
            0xBA, 0x81, 0x23, 0x00, 0x50, 0x00, 0xA0, 0x00, 0x78, 0x01, 0x40, 0x00, 0xF0, 0x02, 0x80, 0x01, 0xE0, 0x9D,
        ];
        let (base, at) = read(&first).unwrap();
        assert_eq!(base.picture, Some(Picture { id: 0x0123, is_long: true }));
        assert_eq!(
            base.layer,
            Some(Layer { temporal: 0, is_switch_up: false, spatial: 0, is_inter_layer: false, tl0: None })
        );
        assert!(base.is_key() && base.is_begin && !base.is_end && base.is_flexible);
        let structure = base.structure.unwrap();
        assert_eq!(
            (structure.spatial_layers, structure.sizes, at),
            (3, vec![(160, 120), (320, 240), (640, 480)], 17),
            "the scalability structure with each layer's size and no picture group"
        );
        let (upper, at) = read(&[0xBC, 0x81, 0x23, 0x03, 0x9D]).unwrap();
        assert_eq!(
            (upper.layer.map(|layer| (layer.spatial, layer.is_inter_layer)), upper.is_key(), upper.is_end, at),
            (Some((1, true)), false, true, 4),
            "the second layer frame of the key picture leans on the first"
        );
    }
}
