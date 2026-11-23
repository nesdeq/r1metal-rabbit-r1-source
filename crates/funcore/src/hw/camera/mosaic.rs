// A frame's square middle as four planes, one for each place of the sensor's colour quads, mirrored past their edges.

use crate::base::fft;
use crate::base::sampling::reflect;
use crate::hw::camera::frame::Frame;
use crate::hw::camera::raw::Raw;
use crate::hw::camera::raw::Square;
use alloc::vec;
use alloc::vec::Vec;

// Samples a plane has past each of its edges, half a tile of the merge, so that every sample lies under two tiles each way.
pub(super) const PAD: usize = fft::SIDE / 2;
// The places of a quad: green and blue on its upper line, red and green on its lower.
pub(super) const PLANES: usize = 4;

// The four planes of a square, each `side` samples along a side and `PAD` more past every edge.
pub(super) struct Planes {
    side: usize,
    samples: [Vec<u16>; PLANES],
}

// Whole rows of the four planes, from row `first` of each.
pub(super) struct Band<'a> {
    first: usize,
    stride: usize,
    side: usize,
    planes: [&'a mut [u16]; PLANES],
}

impl Planes {
    pub(super) fn new(side: usize) -> Planes {
        let stride = side + 2 * PAD;
        Planes { side, samples: core::array::from_fn(|_| vec![0; stride * stride]) }
    }

    pub(super) const fn side(&self) -> usize {
        self.side
    }

    pub(super) const fn stride(&self) -> usize {
        self.side + 2 * PAD
    }

    pub(super) fn plane(&self, plane: usize) -> &[u16] {
        &self.samples[plane]
    }

    // The planes in bands of `rows` rows each.
    pub(super) fn bands(&mut self, rows: usize) -> impl Iterator<Item = Band<'_>> {
        let (stride, side) = (self.stride(), self.side);
        let [first, second, third, fourth] = self.samples.each_mut().map(|plane| plane.chunks_mut(rows * stride));
        first.zip(second).zip(third).zip(fourth).enumerate().map(move |(index, (((first, second), third), fourth))| {
            Band { first: index * rows, stride, side, planes: [first, second, third, fourth] }
        })
    }

    // The sample of the square's pixel at `x` and `y`.
    #[inline]
    pub(super) fn at(&self, x: usize, y: usize) -> u16 {
        let plane = 2 * (y % 2) + x % 2;
        self.samples[plane][(y / 2 + PAD) * self.stride() + x / 2 + PAD]
    }

    // Writes the mean of each place's four samples, rounded, over rows `rows`, into `grey`.
    pub(super) fn grey(&self, rows: core::ops::Range<usize>, grey: &mut [u16]) {
        let range = rows.start * self.stride()..rows.end * self.stride();
        let [first, second, third, fourth] = self.samples.each_ref().map(|plane| &plane[range.clone()]);
        for ((((mean, first), second), third), fourth) in grey.iter_mut().zip(first).zip(second).zip(third).zip(fourth)
        {
            *mean = ((u32::from(*first) + u32::from(*second) + u32::from(*third) + u32::from(*fourth) + 2) / 4) as u16;
        }
    }
}

impl<'a> Band<'a> {
    // Fills the band's rows from the square `square` of a raw frame.
    pub(super) fn unpack(&mut self, raw: &[u8], frame: Frame, square: Square) {
        let raw = Raw::new(raw, frame).expect("a frame of its size");
        let (stride, side) = (self.stride, self.side);
        let mut line = vec![0u16; 2 * side];
        let rows = self.planes[0].len() / stride;
        for row in 0..rows {
            let source = reflect((self.first + row) as isize - PAD as isize, side);
            for upper in 0..2 {
                raw.line(square.top as usize + 2 * source + upper, square.left as usize, &mut line);
                for left in 0..2 {
                    let samples = &mut self.planes[2 * upper + left][row * stride..(row + 1) * stride];
                    for (sample, code) in samples[PAD..PAD + side].iter_mut().zip(line[left..].iter().step_by(2)) {
                        *sample = *code;
                    }
                    for edge in 0..PAD {
                        samples[PAD - 1 - edge] = line[2 * edge + left];
                        samples[PAD + side + edge] = line[2 * (side - 1 - edge) + left];
                    }
                }
            }
        }
    }

    // The band's rows of each plane, to write.
    pub(super) fn rows_mut(&mut self) -> &mut [&'a mut [u16]; PLANES] {
        &mut self.planes
    }

    pub(super) const fn first(&self) -> usize {
        self.first
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::frame::fake::packed;

    #[test]
    fn planes_of_quads() {
        let frame = Frame::whole(24, 16);
        let square = Square::middle_of(frame);
        let bytes = packed(frame, |x, y| 50 * y + x);
        let mut planes = Planes::new(square.side as usize / 2);
        for mut band in planes.bands(5) {
            band.unpack(&bytes, frame, square);
        }
        let side = planes.side();
        assert_eq!((side, planes.stride()), (8, 8 + 2 * PAD));
        for y in 0..2 * side {
            for x in 0..2 * side {
                assert_eq!(
                    u32::from(planes.at(x, y)),
                    50 * y as u32 + x as u32 + square.left,
                    "pixel {x} {y} of the square, from its place in the frame"
                );
            }
        }
        let blue = planes.plane(1);
        let stride = planes.stride();
        assert_eq!(
            (blue[PAD * stride + PAD - 1], blue[PAD * stride + PAD], blue[(PAD - 1) * stride + PAD]),
            (blue[PAD * stride + PAD], 1 + square.left as u16, blue[PAD * stride + PAD]),
            "past an edge a plane mirrors its first sample"
        );
        assert_eq!(blue[PAD * stride + PAD + side], blue[PAD * stride + PAD + side - 1], "and its last");
        let mut grey = vec![0u16; stride];
        planes.grey(PAD..PAD + 1, &mut grey);
        let quad = [planes.at(0, 0), planes.at(1, 0), planes.at(0, 1), planes.at(1, 1)].map(u32::from);
        assert_eq!(u32::from(grey[PAD]), (quad.iter().sum::<u32>() + 2) / 4, "a place's grey is its quad's mean");
    }
}
