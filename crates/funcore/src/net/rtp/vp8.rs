use alloc::vec::Vec;

// RFC 7741, 4.2: the first byte's X, N and S bits and partition index, the extension byte's I, L, T and K bits, the picture id's M bit, and TID, Y and KEYIDX in their byte.
const EXTENDED: u8 = 0x80;
const NON_REFERENCE: u8 = 0x20;
const START: u8 = 0x10;
const PARTITION: u8 = 0x07;
const PICTURE: u8 = 0x80;
const TL0: u8 = 0x40;
const TID: u8 = 0x20;
const KEY_INDEX: u8 = 0x10;
const LONG_PICTURE: u8 = 0x80;
const SHORT_PICTURE: u8 = 0x7F;
const TID_SHIFT: u8 = 6;
const SYNC: u8 = 0x20;
const KEY_INDEX_BITS: u8 = 0x1F;
// RFC 7741, 4.2 and RFC 9628, 4.2: a picture id of 7 bits, or of 15 after the M bit.
const SHORT_PICTURE_BITS: u32 = 7;
const LONG_PICTURE_BITS: u32 = 15;

// What a VP8 payload descriptor tells (RFC 7741, 4.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Descriptor {
    pub(super) is_non_reference: bool,
    pub(super) is_start: bool,
    pub(super) partition: u8,
    pub(super) picture: Option<Picture>,
    pub(super) tl0: Option<u8>,
    pub(super) temporal: Option<Temporal>,
    pub(super) key_index: Option<u8>,
}

// A picture id, as VP8's and VP9's descriptors carry it, and whether it counts in 15 bits or in 7.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Picture {
    pub(crate) id: u16,
    pub(super) is_long: bool,
}

impl Picture {
    pub(crate) fn bits(self) -> u32 {
        if self.is_long { LONG_PICTURE_BITS } else { SHORT_PICTURE_BITS }
    }
}

// A frame's temporal layer and its Y bit, whether it depends on the base layer frame of its TL0PICIDX alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Temporal {
    index: u8,
    is_sync: bool,
}

// What a frame is cut into packets by: the bytes of payload a packet carries at most, and what its first, last or only packet has less for its larger header.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Budget {
    pub(super) bytes: usize,
    pub(super) first_less: usize,
    pub(super) last_less: usize,
    pub(super) single_less: usize,
}

impl Descriptor {
    // The descriptor at a payload's start and the VP8 data after it; none where the descriptor is cut short or no data follows, as libwebrtc refuses (video_rtp_depacketizer_vp8.cc, `ParseRtpPayload`).
    pub(crate) fn read(payload: &[u8]) -> Option<(Descriptor, &[u8])> {
        let (&first, mut rest) = payload.split_first()?;
        let mut descriptor = Descriptor {
            is_non_reference: first & NON_REFERENCE != 0,
            is_start: first & START != 0,
            partition: first & PARTITION,
            ..Descriptor::default()
        };
        if first & EXTENDED != 0 {
            let (&extension, after) = rest.split_first()?;
            rest = after;
            if extension & PICTURE != 0 {
                let (&high, after) = rest.split_first()?;
                rest = after;
                descriptor.picture = Some(if high & LONG_PICTURE == 0 {
                    Picture { id: u16::from(high), is_long: false }
                } else {
                    let (&low, after) = rest.split_first()?;
                    rest = after;
                    Picture { id: u16::from_be_bytes([high & SHORT_PICTURE, low]), is_long: true }
                });
            }
            if extension & TL0 != 0 {
                let (&tl0, after) = rest.split_first()?;
                rest = after;
                descriptor.tl0 = Some(tl0);
            }
            if extension & (TID | KEY_INDEX) != 0 {
                let (&byte, after) = rest.split_first()?;
                rest = after;
                descriptor.temporal =
                    (extension & TID != 0).then_some(Temporal { index: byte >> TID_SHIFT, is_sync: byte & SYNC != 0 });
                descriptor.key_index = (extension & KEY_INDEX != 0).then_some(byte & KEY_INDEX_BITS);
            }
        }
        (!rest.is_empty()).then_some((descriptor, rest))
    }

    // Appends the descriptor as libwebrtc writes it (rtp_format_vp8.cc, `BuildHeader`), a picture id of 7 bits where it is one.
    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        let extension = if self.picture.is_some() { PICTURE } else { 0 }
            | if self.tl0.is_some() { TL0 } else { 0 }
            | if self.temporal.is_some() { TID } else { 0 }
            | if self.key_index.is_some() { KEY_INDEX } else { 0 };
        out.push(
            if extension != 0 { EXTENDED } else { 0 }
                | if self.is_non_reference { NON_REFERENCE } else { 0 }
                | if self.is_start { START } else { 0 }
                | self.partition & PARTITION,
        );
        if extension == 0 {
            return;
        }
        out.push(extension);
        match self.picture {
            Some(Picture { id, is_long: true }) => {
                let [high, low] = id.to_be_bytes();
                out.extend([LONG_PICTURE | high & SHORT_PICTURE, low]);
            }
            Some(Picture { id, is_long: false }) => out.push(id as u8 & SHORT_PICTURE),
            None => {}
        }
        out.extend(self.tl0);
        if self.temporal.is_some() || self.key_index.is_some() {
            let temporal = self
                .temporal
                .map_or(0, |temporal| temporal.index << TID_SHIFT | if temporal.is_sync { SYNC } else { 0 });
            out.push(temporal | self.key_index.unwrap_or(0) & KEY_INDEX_BITS);
        }
    }

    // The bytes `write` appends.
    pub(crate) fn len(&self) -> usize {
        let mut out = Vec::new();
        self.write(&mut out);
        out.len()
    }
}

// The bytes of a frame each packet carries, as libwebrtc's `SplitAboutEqually` cuts them (rtp_format.cc): one packet where it fits, else as few as fit, of sizes that differ by a byte at most once the first and last packets' less room is counted; none where a byte fits in no packet.
pub(super) fn split(length: usize, budget: Budget) -> Vec<usize> {
    if length == 0 {
        return Vec::new();
    }
    if budget.bytes >= budget.single_less + length {
        return alloc::vec![length];
    }
    if budget.bytes <= budget.first_less || budget.bytes <= budget.last_less {
        return Vec::new();
    }
    let total = length + budget.first_less + budget.last_less;
    let count = total.div_ceil(budget.bytes).max(2);
    if length < count {
        return Vec::new();
    }
    let (mut each, larger) = (total / count, total % count);
    let (mut left, mut remaining) = (count, length);
    let mut sizes = Vec::with_capacity(count);
    while remaining > 0 {
        if left == larger {
            each += 1;
        }
        let mut size = each;
        if sizes.is_empty() {
            size = if size > budget.first_less + 1 { size - budget.first_less } else { 1 };
        }
        size = size.min(remaining);
        if left == 2 && size == remaining {
            size -= 1;
        }
        sizes.push(size);
        remaining -= size;
        left -= 1;
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(payload: &[u8]) -> Option<(Descriptor, usize)> {
        Descriptor::read(payload).map(|(descriptor, rest)| (descriptor, payload.len() - rest.len()))
    }

    const fn long(id: u16) -> Option<Picture> {
        Some(Picture { id, is_long: true })
    }

    // libwebrtc's video_rtp_depacketizer_vp8_unittest.cc, case by case.
    #[test]
    fn libwebrtc_depacketizer_cases() {
        let basic = read(&[0b0001_0100, 0x01, 0, 0]).unwrap();
        assert_eq!(
            basic,
            (Descriptor { is_start: true, partition: 4, ..Descriptor::default() }, 1),
            "BasicHeader: S, partition 4, nothing else"
        );
        let mut packet = [0u8; 10];
        packet[..3].copy_from_slice(&[0b1010_0000, 0b1000_0000, 17]);
        assert_eq!(
            read(&packet).map(|(read, at)| (read.picture, at)),
            Some((Some(Picture { id: 17, is_long: false }), 3)),
            "OneBytePictureID"
        );
        packet[..4].copy_from_slice(&[0b1010_0000, 0b1000_0000, 0x80 | 0x12, 0x34]);
        assert_eq!(read(&packet).map(|(read, at)| (read.picture, at)), Some((long(0x1234), 4)), "TwoBytePictureID");
        let mut packet = [0u8; 13];
        packet[..3].copy_from_slice(&[0b1000_0000, 0b0100_0000, 17]);
        assert_eq!(read(&packet).map(|(read, at)| (read.tl0, at)), Some((Some(17), 3)), "Tl0PicIdx");
        let mut packet = [0u8; 10];
        packet[..3].copy_from_slice(&[0b1000_0000, 0b0010_0000, 0b1000_0000]);
        assert_eq!(
            read(&packet).map(|(read, at)| (read.temporal, at)),
            Some((Some(Temporal { index: 2, is_sync: false }), 3)),
            "TIDAndLayerSync"
        );
        packet[..3].copy_from_slice(&[0b1000_0000, 0b0001_0000, 17]);
        assert_eq!(
            read(&packet).map(|(read, at)| (read.key_index, read.temporal, at)),
            Some((Some(17), None, 3)),
            "KeyIdx"
        );
        packet[..6].copy_from_slice(&[0b1010_0110, 0b1111_0000, 0x80 | 0x12, 0x34, 42, 0b0111_0001]);
        assert_eq!(
            read(&packet),
            Some((
                Descriptor {
                    is_non_reference: true,
                    is_start: false,
                    partition: 6,
                    picture: long(0x1234),
                    tl0: Some(42),
                    temporal: Some(Temporal { index: 1, is_sync: true }),
                    key_index: Some(0b10001),
                },
                6
            )),
            "MultipleExtensions"
        );
        assert_eq!(read(&[0b1000_0000, 0b1111_0000, 0x80 | 17, 17]), None, "TooShortHeader");
        let mut packet = [0u8; 16];
        packet[..3].copy_from_slice(&[0b1000_0000, 0b1111_0000, 15]);
        assert_eq!(
            read(&packet).map(|(_, at)| at),
            Some(5),
            "ReferencesInputCopyOnWriteBuffer: five bytes of descriptor"
        );
        assert_eq!(read(&[]), None, "FailsOnEmptyPayload");
        assert_eq!(read(&[0x10]), None, "a descriptor with no VP8 data after it");
    }

    // libwebrtc's WithPacketizer and rtp_format_vp8_test_helper.cc: what the packetizer writes reads back.
    #[test]
    fn written_and_read_back() {
        let cases = [
            Descriptor {
                is_non_reference: true,
                is_start: true,
                picture: long(300),
                temporal: Some(Temporal { index: 1, is_sync: false }),
                key_index: Some(31),
                ..Descriptor::default()
            },
            Descriptor { is_start: true, picture: long(0x7FFF), ..Descriptor::default() },
            Descriptor { picture: Some(Picture { id: 0x55, is_long: false }), ..Descriptor::default() },
            Descriptor {
                tl0: Some(117),
                temporal: Some(Temporal { index: 2, is_sync: true }),
                ..Descriptor::default()
            },
            Descriptor { key_index: Some(17), ..Descriptor::default() },
            Descriptor { is_start: true, ..Descriptor::default() },
        ];
        for descriptor in cases {
            let mut out = Vec::new();
            descriptor.write(&mut out);
            assert_eq!(out.len(), descriptor.len());
            out.extend([0xAA; 3]);
            assert_eq!(read(&out), Some((descriptor, descriptor.len())), "{descriptor:?}");
        }
        let mut out = Vec::new();
        Descriptor { is_start: true, picture: long(200), ..Descriptor::default() }.write(&mut out);
        assert_eq!(out, [0x90, 0x80, 0x80, 200], "X and S, I, a picture id of two bytes with M");
        let mut out = Vec::new();
        Descriptor { is_start: true, is_non_reference: true, ..Descriptor::default() }.write(&mut out);
        assert_eq!(
            out,
            [0x30],
            "N and S alone, the one byte left under a dependency descriptor (rtp_sender_video.cc:91-102)"
        );
    }

    // libwebrtc's rtp_format_vp8_unittest.cc: each case's payload sizes, its four bytes of descriptor taken off.
    #[test]
    fn libwebrtc_packetizer_sizes() {
        let budget = |bytes, last_less| Budget { bytes, last_less, ..Budget::default() };
        assert_eq!(split(30, budget(12 - 4, 0)), [7, 7, 8, 8], "ResultPacketsAreAlmostEqualSize: 11, 11, 12, 12");
        assert_eq!(
            split(43, budget(15 - 4, 5)),
            [9, 9, 10, 10, 5],
            "EqualSizeWithLastPacketReduction: 13, 13, 14, 14, 9"
        );
        assert_eq!(split(30, budget(25 - 1, 0)), [15, 15], "NonReferenceBit: 16, 16 with one byte of descriptor");
        assert!(split(0, budget(8, 0)).is_empty(), "EmptyPayload");
        assert_eq!(split(30, budget(1200, 0)), [30], "one packet");
        let first = Budget { bytes: 100, first_less: 40, last_less: 10, single_less: 50 };
        let sizes = split(500, first);
        assert_eq!(sizes.iter().sum::<usize>(), 500, "every byte once");
        assert!(sizes[0] <= 60 && sizes[sizes.len() - 1] <= 90 && sizes.iter().all(|size| *size <= 100));
        assert_eq!(split(60, first), [15, 45], "it would fit alone but for the single packet's larger header");
        assert!(split(5, Budget { bytes: 4, first_less: 4, ..Budget::default() }).is_empty(), "no room in the first");
        assert!(
            split(1, Budget { bytes: 2, first_less: 1, last_less: 1, single_less: 2 }).is_empty(),
            "more packets than bytes"
        );
    }
}
