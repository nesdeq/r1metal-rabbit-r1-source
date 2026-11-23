use crate::base::proto::write_varint;
use alloc::vec::Vec;

// webrtc.org's video-layers-allocation00: the header byte's RID, NS less one and spatial layer bitmask, a stream's bitmask of four bits, and two bits a layer of its temporal layers less one; up to four streams of four spatial layers and four temporal layers each.
const RID_SHIFT: u8 = 6;
const STREAMS_SHIFT: u8 = 4;
const MASK_BITS: u8 = 4;
const TEMPORAL_BITS: u8 = 2;
const MOST: u8 = 4;
// The one byte of an allocation of no active layer.
const NONE: u8 = 0;

// An active spatial layer: its RTP stream, its spatial id, the kbit/s each temporal layer needs with the layers below it, and its size and most frames a second where told.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Layer {
    stream: u8,
    spatial: u8,
    rates: Vec<u32>,
    size: Option<Size>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Size {
    width: u16,
    height: u16,
    frame_rate: u8,
}

// An allocation as a packet of one RTP stream tells it: that stream's index, and every active layer in the order of stream and spatial id.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Allocation {
    stream: u8,
    layers: Vec<Layer>,
}

impl Allocation {
    // The allocation of this device's one stream of one layer and one temporal layer, sent on the first of the calling server's layers.
    pub(super) fn single(kbps: u32, width: u16, height: u16, frame_rate: u8) -> Allocation {
        let layer =
            Layer { rates: alloc::vec![kbps], size: Some(Size { width, height, frame_rate }), ..Layer::default() };
        Allocation { stream: 0, layers: alloc::vec![layer] }
    }

    // Appends the allocation, as libwebrtc writes it (rtp_video_layers_allocation_extension.cc, `Write`); false, appending nothing, for one libwebrtc holds invalid (`AllocationIsValid`).
    pub(crate) fn write(&self, out: &mut Vec<u8>) -> bool {
        let is_sorted = self.layers.is_sorted_by_key(|layer| (layer.stream, layer.spatial));
        let streams = self.layers.iter().map(|layer| layer.stream).max().unwrap_or(0) + 1;
        let is_valid = is_sorted
            && self.layers.iter().all(|layer| {
                layer.stream < MOST
                    && layer.spatial < MOST
                    && (1..=usize::from(MOST)).contains(&layer.rates.len())
                    && layer.size.is_some() == self.layers[0].size.is_some()
                    && layer.size.is_none_or(|size| size.width > 0 && size.height > 0)
            })
            && (self.layers.is_empty() || self.stream < streams);
        if !is_valid {
            return false;
        }
        if self.layers.is_empty() {
            out.push(NONE);
            return true;
        }
        let mut masks = [0u8; MOST as usize];
        for layer in &self.layers {
            masks[usize::from(layer.stream)] |= 1 << layer.spatial;
        }
        let is_shared = masks[..usize::from(streams)].iter().all(|mask| *mask == masks[0]);
        let shared = if is_shared { masks[0] } else { 0 };
        out.push(self.stream << RID_SHIFT | (streams - 1) << STREAMS_SHIFT | shared);
        if !is_shared {
            out.push(masks[0] << MASK_BITS | masks[1]);
            if streams > 2 {
                out.push(masks[2] << MASK_BITS | masks[3]);
            }
        }
        for counts in self.layers.chunks(usize::from(u8::BITS as u8 / TEMPORAL_BITS)) {
            let byte = counts.iter().enumerate().fold(0u8, |byte, (at, layer)| {
                byte | (layer.rates.len() as u8 - 1) << (u8::BITS as u8 - TEMPORAL_BITS * (at as u8 + 1))
            });
            out.push(byte);
        }
        self.layers.iter().flat_map(|layer| &layer.rates).for_each(|rate| write_varint(out, u64::from(*rate)));
        for size in self.layers.iter().filter_map(|layer| layer.size) {
            out.extend((size.width - 1).to_be_bytes());
            out.extend((size.height - 1).to_be_bytes());
            out.push(size.frame_rate);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::proto::read_varint;
    use alloc::vec;

    // libwebrtc's reader of the extension (rtp_video_layers_allocation_extension.cc, `Parse`), for its tests' round trips; rates past a million kbit/s refused as it refuses them.
    fn parse(bytes: &[u8]) -> Option<Allocation> {
        const MOST_RATE: u64 = 1_000_000;
        const MASK: u8 = (1 << MASK_BITS) - 1;
        const COUNT: u8 = (1 << TEMPORAL_BITS) - 1;
        const PER_BYTE: usize = (u8::BITS as u8 / TEMPORAL_BITS) as usize;
        // A layer's width and height less one, two bytes each, and its frame rate.
        const SIZE_BYTES: usize = 5;
        if bytes == [NONE] {
            return Some(Allocation::default());
        }
        let (&header, mut rest) = bytes.split_first()?;
        let streams = 1 + (header >> STREAMS_SHIFT & COUNT);
        let mut masks = [header & MASK; MOST as usize];
        if masks[0] == 0 {
            let (&pair, after) = rest.split_first()?;
            (masks[0], masks[1], rest) = (pair >> MASK_BITS, pair & MASK, after);
            if streams > 2 {
                let (&pair, after) = rest.split_first()?;
                (masks[2], masks[3], rest) = (pair >> MASK_BITS, pair & MASK, after);
            }
        }
        let mut layers = Vec::new();
        for stream in 0..streams {
            for spatial in (0..MOST).filter(|spatial| masks[usize::from(stream)] >> spatial & 1 == 1) {
                layers.push(Layer { stream, spatial, ..Layer::default() });
            }
        }
        for (at, layer) in layers.iter_mut().enumerate() {
            let byte = *rest.get(at / PER_BYTE)?;
            let shift = u8::BITS as u8 - TEMPORAL_BITS * ((at % PER_BYTE) as u8 + 1);
            layer.rates = vec![0; usize::from(1 + (byte >> shift & COUNT))];
        }
        rest = rest.get(layers.len().div_ceil(PER_BYTE)..)?;
        for rate in layers.iter_mut().flat_map(|layer| &mut layer.rates) {
            let (value, read) = read_varint(rest).filter(|(value, _)| *value <= MOST_RATE)?;
            (*rate, rest) = (value as u32, &rest[read..]);
        }
        if !rest.is_empty() {
            if rest.len() != SIZE_BYTES * layers.len() {
                return None;
            }
            for (layer, size) in layers.iter_mut().zip(rest.chunks(SIZE_BYTES)) {
                let width = u16::from_be_bytes([size[0], size[1]]).checked_add(1)?;
                let height = u16::from_be_bytes([size[2], size[3]]).checked_add(1)?;
                layer.size = Some(Size { width, height, frame_rate: size[4] });
            }
        }
        Some(Allocation { stream: header >> RID_SHIFT, layers })
    }

    fn layer(stream: u8, spatial: u8, rates: &[u32], size: Option<(u16, u16, u8)>) -> Layer {
        let size = size.map(|(width, height, frame_rate)| Size { width, height, frame_rate });
        Layer { stream, spatial, rates: rates.to_vec(), size }
    }

    // libwebrtc's rtp_video_layers_allocation_extension_unittest.cc: each allocation written and read back.
    #[test]
    fn libwebrtc_round_trips() {
        let cases = [
            ("CanWriteAndParseLayersAllocationWithZeroSpatialLayers", Allocation::default()),
            (
                "CanWriteAndParse2SpatialWith2TemporalLayers",
                Allocation { stream: 1, layers: vec![layer(0, 0, &[25, 50], None), layer(1, 0, &[100, 200], None)] },
            ),
            (
                "CanWriteAndParseAllocationWithDifferentNumerOfSpatialLayers",
                Allocation {
                    stream: 1,
                    layers: vec![layer(0, 0, &[50], None), layer(1, 0, &[100], None), layer(1, 1, &[200], None)],
                },
            ),
            (
                "CanWriteAndParseAllocationWithSkippedLowerSpatialLayer",
                Allocation { stream: 1, layers: vec![layer(0, 0, &[50], None), layer(1, 1, &[200], None)] },
            ),
            (
                "CanWriteAndParseAllocationWithSkippedRtpStreamIds",
                Allocation { stream: 2, layers: vec![layer(0, 0, &[50], None), layer(2, 0, &[200], None)] },
            ),
            (
                "CanWriteAndParseAllocationWithDifferentNumerOfTemporalLayers",
                Allocation { stream: 1, layers: vec![layer(0, 0, &[25, 50], None), layer(1, 0, &[100], None)] },
            ),
            (
                "CanWriteAndParseAllocationWithResolution",
                Allocation {
                    stream: 1,
                    layers: vec![
                        layer(0, 0, &[25, 50], Some((320, 240, 8))),
                        layer(1, 1, &[100, 200], Some((640, 320, 30))),
                    ],
                },
            ),
        ];
        for (name, allocation) in cases {
            let mut out = Vec::new();
            assert!(allocation.write(&mut out), "{name}: written");
            assert_eq!(parse(&out), Some(allocation), "{name}: read back");
        }
        let mut out = Vec::new();
        assert!(
            Allocation { stream: 1, layers: vec![] }.write(&mut out) && out == [0],
            "WriteEmptyAllocationCanHaveAnyRtpStreamIndex"
        );
        let large = [0x4b, 0xf6, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xcb, 0x78, 0xeb, 0x8d, 0xb5, 0x31];
        assert_eq!(parse(&large), None, "DiscardsOverLargeDataRate");
        let mut sized = Vec::new();
        assert!(Allocation { stream: 0, layers: vec![layer(0, 0, &[25, 50], Some((320, 240, 8)))] }.write(&mut sized));
        let at = sized.len() - 3;
        sized[at..at + 2].copy_from_slice(&[0xFF, 0xFF]);
        assert_eq!(parse(&sized), None, "DiscardsInvalidHeight: a height of 65536");
    }

    // The calling server's packet.rs tests of the extension it reads, written byte for byte.
    #[test]
    fn calling_server_vectors() {
        let three = Allocation {
            stream: 2,
            layers: vec![
                layer(0, 0, &[30, 45, 75], Some((160, 120, 23))),
                layer(1, 0, &[81, 122, 203], Some((320, 240, 23))),
                layer(2, 0, &[265, 398, 663], Some((640, 480, 23))),
            ],
        };
        let mut out = Vec::new();
        assert!(three.write(&mut out));
        assert_eq!(
            out,
            [
                0xA1, 0xA8, 30, 45, 75, 81, 122, 0xCB, 0x01, 0x89, 0x02, 0x8E, 0x03, 0x97, 0x05, 0, 159, 0, 119, 23, 1,
                63, 0, 239, 23, 2, 127, 1, 223, 23
            ],
            "read_video_three_layers_with_size"
        );
        let one = Allocation { stream: 0, layers: vec![layer(0, 0, &[35, 53, 88], Some((160, 120, 22)))] };
        let mut out = Vec::new();
        assert!(
            one.write(&mut out) && out == [0x01, 0x80, 0x23, 0x35, 0x58, 0x00, 0x9F, 0x00, 0x77, 22],
            "read_video_one_layer"
        );
        let bare = Allocation { stream: 0, layers: vec![layer(0, 0, &[35, 53, 88], None)] };
        let mut out = Vec::new();
        assert!(bare.write(&mut out) && out == [0x01, 0x80, 0x23, 0x35, 0x58], "read_video_one_layer_no_size");
        let mut out = Vec::new();
        assert!(Allocation::single(700, 640, 480, 30).write(&mut out));
        assert_eq!(
            out,
            [0x01, 0x00, 0xBC, 0x05, 0x02, 0x7F, 0x01, 0xDF, 30],
            "this device's one layer: RID 0 of one stream, one temporal layer at 700 kbit/s, 640x480 at 30 frames a second"
        );
    }

    #[test]
    fn invalid_allocations_refused() {
        for (name, allocation) in [
            ("unsorted", Allocation { stream: 1, layers: vec![layer(1, 0, &[1], None), layer(0, 0, &[1], None)] }),
            ("a fifth stream", Allocation { stream: 0, layers: vec![layer(4, 0, &[1], None)] }),
            ("no rate", Allocation { stream: 0, layers: vec![layer(0, 0, &[], None)] }),
            ("five temporal layers", Allocation { stream: 0, layers: vec![layer(0, 0, &[1; 5], None)] }),
            (
                "sizes on some layers alone",
                Allocation { stream: 1, layers: vec![layer(0, 0, &[1], None), layer(1, 0, &[1], Some((2, 2, 1)))] },
            ),
            ("a width of none", Allocation { stream: 0, layers: vec![layer(0, 0, &[1], Some((0, 2, 1)))] }),
            ("a stream past the last", Allocation { stream: 1, layers: vec![layer(0, 0, &[1], None)] }),
        ] {
            let mut out = vec![9];
            assert!(!allocation.write(&mut out) && out == [9], "{name}");
        }
    }
}
