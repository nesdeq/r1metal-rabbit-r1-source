//! The sensor's raw frames as CAMSV0 lays them into memory: their size, their packing and the codes of black and white.

use crate::base::simd::CODE_BITS;

pub(super) const BLACK: u32 = 64;
pub(super) const WHITE: u32 = 1023;
pub(super) const RAW_CODES: usize = WHITE as usize + 1;
const LINE_ALIGNMENT: u32 = 4;

/// The part of the sensor's frame written to memory: pixels `left` to `right` of every line, `lines` lines from the top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame {
    pub(super) left: u32,
    pub(super) right: u32,
    pub(super) lines: u32,
}

// The sensor's every pixel.
pub(super) const SENSOR: Frame = Frame::whole(3264, 2448);

impl Frame {
    pub(super) const fn whole(width: u32, lines: u32) -> Frame {
        Frame { left: 0, right: width, lines }
    }

    pub(crate) const fn width(&self) -> u32 {
        self.right - self.left
    }

    pub(crate) const fn line_bytes(&self) -> u32 {
        (self.width() * CODE_BITS as u32).div_ceil(u8::BITS).next_multiple_of(LINE_ALIGNMENT)
    }

    /// Bytes of the frame in memory.
    pub const fn bytes(&self) -> usize {
        self.line_bytes() as usize * self.lines as usize
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;
    use crate::base::bits::BYTE_BITS;
    use crate::base::simd::CODE_MASK;
    use alloc::vec;
    use alloc::vec::Vec;

    pub(crate) fn packed(frame: Frame, pixel: impl Fn(u32, u32) -> u32) -> Vec<u8> {
        let stride = frame.line_bytes() as usize;
        let mut bytes = vec![0u8; frame.bytes()];
        for y in 0..frame.lines {
            for x in 0..frame.width() {
                let bit = x as usize * CODE_BITS;
                let at = y as usize * stride + bit / BYTE_BITS;
                let word = (pixel(x, y) & u32::from(CODE_MASK)) << (bit % BYTE_BITS);
                bytes[at] |= word as u8;
                bytes[at + 1] |= (word >> u8::BITS) as u8;
            }
        }
        bytes
    }

    pub(crate) fn flat(frame: Frame, [red, green, blue]: [u32; 3]) -> Vec<u8> {
        packed(frame, |x, y| match (x % 2, y % 2) {
            (1, 0) => blue,
            (0, 1) => red,
            _ => green,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_packing() {
        let slim = Frame::whole(1280, 720);
        assert_eq!((slim.line_bytes(), slim.bytes()), (1600, 1_152_000));
        assert_eq!((Frame::whole(3264, 2448).line_bytes(), Frame::whole(3264, 2448).bytes()), (4080, 9_987_840));
        assert_eq!(Frame::whole(1632, 1224).line_bytes(), 2040);
        assert_eq!(Frame { left: 640, right: 1280, lines: 720 }.line_bytes(), 800);
        assert_eq!(Frame::whole(1282, 1).line_bytes(), 1604, "a line that ends inside a word is padded");
    }
}
