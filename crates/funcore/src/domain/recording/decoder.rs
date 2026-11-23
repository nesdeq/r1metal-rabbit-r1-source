use super::{BLOCK, CHROMA, COUNT_AT, ENTRY_BYTES, FRAME_HEAD_BYTES, PLACE_BYTES, frame_size, is_recordable};
use crate::base::bytes::le16;
use crate::base::ycbcr::{CHROMA_MIDDLE, LUMA_FLOOR};
use alloc::vec;
use alloc::vec::Vec;

/// A frame body of another size, or with blocks cut short or outside it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Malformed;

/// Rebuilds a recording's frames as I420 planes.
pub struct Decoder {
    width: usize,
    height: usize,
    planes: Vec<u8>,
}

impl Decoder {
    /// A black picture of that size; none unless recordable.
    pub fn new(width: usize, height: usize) -> Option<Decoder> {
        if !is_recordable(width, height) {
            return None;
        }
        let lumas = width * height;
        let mut planes = vec![LUMA_FLOOR as u8; lumas + lumas / 2];
        planes[lumas..].fill(CHROMA_MIDDLE as u8);
        Some(Decoder { width, height, planes })
    }

    /// The current picture.
    pub fn planes(&self) -> &[u8] {
        &self.planes
    }

    /// Applies a frame body's blocks to the picture.
    pub fn apply(&mut self, body: &[u8]) -> Result<(), Malformed> {
        if body.len() < FRAME_HEAD_BYTES || frame_size(body) != Some((self.width, self.height)) {
            return Err(Malformed);
        }
        let count = usize::from(le16(body, COUNT_AT).ok_or(Malformed)?);
        if body.len() != FRAME_HEAD_BYTES + count * ENTRY_BYTES {
            return Err(Malformed);
        }
        let (across, down) = (self.width / BLOCK, self.height / BLOCK);
        let (lumas, chroma_width) = (self.width * self.height, self.width / 2);
        for entry in body[FRAME_HEAD_BYTES..].as_chunks::<ENTRY_BYTES>().0 {
            let place = usize::from(le16(entry, 0).ok_or(Malformed)?);
            if place >= across * down {
                return Err(Malformed);
            }
            let (left, top) = (place % across * BLOCK, place / across * BLOCK);
            let samples = &entry[PLACE_BYTES..];
            for line in 0..BLOCK {
                self.planes[(top + line) * self.width + left..][..BLOCK]
                    .copy_from_slice(&samples[line * BLOCK..][..BLOCK]);
            }
            let (blues, reds) = samples[BLOCK * BLOCK..].split_at(CHROMA * CHROMA);
            for line in 0..CHROMA {
                let row = (top / 2 + line) * chroma_width + left / 2;
                self.planes[lumas + row..][..CHROMA].copy_from_slice(&blues[line * CHROMA..][..CHROMA]);
                self.planes[lumas + lumas / 4 + row..][..CHROMA].copy_from_slice(&reds[line * CHROMA..][..CHROMA]);
            }
        }
        Ok(())
    }
}
