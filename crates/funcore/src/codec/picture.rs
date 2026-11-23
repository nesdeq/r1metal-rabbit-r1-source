// Decoded pictures: frames of planes inside borders that repeat their edges, and the picture lent of one.

use crate::codec::Flaw;
use alloc::vec::Vec;

// The picture bound: H.264 level 4.1's 8192 macroblocks, 256 across or down (Table A-1, A.3.1), as the device records.
const MOST_MACROBLOCKS: usize = 8192;
const MOST_ACROSS: usize = 256;
const MACROBLOCK: usize = 16;

// A decoded picture, lent until the decoder's next call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Picture<'a> {
    /// Visible pixels across.
    pub width: usize,
    // Visible lines.
    pub(crate) height: usize,
    // The Y plane and the U and V planes of half the width and height, rounded up, each from its first visible pixel.
    pub(crate) planes: [&'a [u8]; 3],
    // Bytes from the start of a plane's line to the start of the next.
    pub(crate) strides: [usize; 3],
}

// Refuses a picture larger than the device decodes, before anything of its size is allocated.
pub(super) fn check_size(width: usize, height: usize) -> Result<(), Flaw> {
    let (columns, rows) = (width.div_ceil(MACROBLOCK), height.div_ceil(MACROBLOCK));
    if columns > MOST_ACROSS || rows > MOST_ACROSS || columns * rows > MOST_MACROBLOCKS {
        return Err(Flaw::Unsupported("a picture of more than 8192 macroblocks or 256 across or down"));
    }
    Ok(())
}

// Where a plane's pixels lie: an area inside a border that repeats its edges.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Geometry {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) border: usize,
    pub(super) stride: usize,
}

impl Geometry {
    pub(super) const fn new(width: usize, height: usize, border: usize) -> Geometry {
        Geometry { width, height, border, stride: width + 2 * border }
    }

    // The place of the area's pixel at `x`, `y`.
    #[inline]
    pub(super) const fn at(self, x: usize, y: usize) -> usize {
        (self.border + y) * self.stride + self.border + x
    }

    pub(super) const fn bytes(self) -> usize {
        self.stride * (self.height + 2 * self.border)
    }
}

// A frame's Y, U and V planes, each with its border.
#[derive(Default)]
pub(super) struct Frame {
    pub(super) planes: [Vec<u8>; 3],
}

impl Frame {
    pub(super) fn allocate(&mut self, geometries: &[Geometry; 3]) {
        for (plane, geometry) in self.planes.iter_mut().zip(geometries) {
            plane.clear();
            plane.resize(geometry.bytes(), 0);
        }
    }

    // Fills each plane's border with copies of its edge pixels, so a vector may point past the edges.
    pub(super) fn extend(&mut self, geometries: &[Geometry; 3]) {
        for (plane, geometry) in self.planes.iter_mut().zip(geometries) {
            extend_plane(plane, *geometry);
        }
    }

    // The picture of `width` by `height` pixels at each area's top left, chroma of half the size rounded up.
    pub(super) fn picture(&self, geometries: &[Geometry; 3], (width, height): (usize, usize)) -> Picture<'_> {
        let sizes = [(width, height), (width.div_ceil(2), height.div_ceil(2))];
        let planes = core::array::from_fn(|index| {
            let (geometry, (width, height)) = (geometries[index], sizes[usize::from(index > 0)]);
            let start = geometry.at(0, 0);
            self.planes[index].get(start..start + (height - 1) * geometry.stride + width).unwrap_or(&[])
        });
        Picture { width, height, planes, strides: geometries.map(|geometry| geometry.stride) }
    }
}

pub(super) fn extend_plane(plane: &mut [u8], geometry: Geometry) {
    let Geometry { width, height, border, stride } = geometry;
    if width == 0 || height == 0 || plane.len() < geometry.bytes() {
        return;
    }
    for line in plane.chunks_exact_mut(stride).skip(border).take(height) {
        let (first, last) = (line[border], line[border + width - 1]);
        line[..border].fill(first);
        line[border + width..].fill(last);
    }
    let (top, bottom) = (geometry.at(0, 0) - border, geometry.at(0, height - 1) - border);
    for line in 0..border {
        plane.copy_within(top..top + stride, line * stride);
        plane.copy_within(bottom..bottom + stride, bottom + (line + 1) * stride);
    }
}

#[cfg(test)]
pub(super) mod samples {
    use super::Picture;
    use alloc::vec::Vec;

    // The visible bytes of a picture's planes, in the order of an I420 file: Y, then U, then V.
    pub(in crate::codec) fn i420(picture: &Picture<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        let sizes = [(picture.width, picture.height), (picture.width.div_ceil(2), picture.height.div_ceil(2))];
        for (index, (plane, stride)) in picture.planes.iter().zip(picture.strides).enumerate() {
            let (width, height) = sizes[usize::from(index > 0)];
            for line in plane.chunks(stride).take(height) {
                bytes.extend_from_slice(&line[..width]);
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_repeats_edges() {
        let geometry = Geometry::new(3, 2, 2);
        let mut plane = alloc::vec![0u8; geometry.bytes()];
        for (place, value) in [(geometry.at(0, 0), 1), (geometry.at(1, 0), 2), (geometry.at(2, 0), 3)] {
            plane[place] = value;
        }
        for (place, value) in [(geometry.at(0, 1), 4), (geometry.at(1, 1), 5), (geometry.at(2, 1), 6)] {
            plane[place] = value;
        }
        extend_plane(&mut plane, geometry);
        #[rustfmt::skip]
        let expected = [
            1, 1, 1, 2, 3, 3, 3,
            1, 1, 1, 2, 3, 3, 3,
            1, 1, 1, 2, 3, 3, 3,
            4, 4, 4, 5, 6, 6, 6,
            4, 4, 4, 5, 6, 6, 6,
            4, 4, 4, 5, 6, 6, 6,
        ];
        assert_eq!(plane, expected, "the corners repeat the corner pixels, the sides the edge pixels");
    }

    #[test]
    fn size_bound() {
        assert_eq!(check_size(1920, 1088), Ok(()), "8160 macroblocks");
        assert_eq!(check_size(4096, 512), Ok(()), "256 across, 8192 in all");
        assert!(check_size(4097, 16).is_err(), "257 across");
        assert!(check_size(1, 4097).is_err(), "257 down");
        assert!(check_size(2048, 1040).is_err(), "8320 macroblocks");
    }
}
