use super::{
    BLOCK, BLOCK_BYTES, CHROMA, COUNT_AT, HEAD_BYTES, Head, Kind, LENGTH_AT, START_BYTES, is_recordable, largest_record,
};
use crate::base::pixel::PixelFormat;
use crate::base::ycbcr::{CHROMA_MIDDLE, LUMA_FLOOR, Matrix, WEIGHTS_709};
use crate::gfx::refresh::REFRESH_HZ;
use alloc::vec;
use alloc::vec::Vec;

const FRACTION: u32 = 16;
const HALF: i32 = 1 << (FRACTION - 1);
const BT709: Matrix = Matrix::new(WEIGHTS_709, 1 << FRACTION, false);

const fn sample(from: i32, weights: [i32; 3], [r, g, b]: [i32; 3]) -> u8 {
    (from + ((weights[0] * r + weights[1] * g + weights[2] * b + HALF) >> FRACTION)) as u8
}

/// Encodes frames as records of changed blocks, the first after a start whole.
pub struct Encoder {
    width: usize,
    height: usize,
    format: PixelFormat,
    last: Vec<u32>,
    is_whole: bool,
    record: Vec<u8>,
    changes: Vec<bool>,
}

impl Encoder {
    /// An encoder of frames of that size; none unless recordable.
    pub fn new(width: usize, height: usize, format: PixelFormat) -> Option<Encoder> {
        if !is_recordable(width, height) {
            return None;
        }
        Some(Encoder {
            width,
            height,
            format,
            last: vec![0; width * height],
            is_whole: true,
            record: Vec::with_capacity(largest_record(width, height)),
            changes: vec![false; width / BLOCK],
        })
    }

    /// The record that begins a recording at `time_us`; the next frame goes whole.
    pub fn start(&mut self, time_us: u64) -> &[u8] {
        self.is_whole = true;
        self.record.clear();
        Head { kind: Kind::Start, time_us, length: START_BYTES as u32, skips: 0 }.write(&mut self.record);
        for value in [self.width as u16, self.height as u16, REFRESH_HZ as u16, 0] {
            self.record.extend_from_slice(&value.to_le_bytes());
        }
        &self.record
    }

    /// The record of a frame shown from `time_us` on; none for too few pixels.
    pub fn frame(&mut self, pixels: &[u32], stride: usize, time_us: u64, skips: u32) -> Option<&[u8]> {
        if stride < self.width || pixels.len() < stride * (self.height - 1) + self.width {
            return None;
        }
        let across = self.width / BLOCK;
        self.record.clear();
        Head { kind: Kind::Frame, time_us, length: 0, skips }.write(&mut self.record);
        for value in [self.width, self.height, 0, 0] {
            self.record.extend_from_slice(&(value as u16).to_le_bytes());
        }
        let mut count = 0u16;
        // Band by band, line after line: the order memory reads fastest, and a band's blocks are made while it is cached.
        for band in 0..self.height / BLOCK {
            let lines = band * BLOCK..(band + 1) * BLOCK;
            self.changes.fill(self.is_whole);
            for line in lines.clone() {
                let (new, old) =
                    (&pixels[line * stride..][..self.width], &self.last[line * self.width..][..self.width]);
                for (column, is_changed) in self.changes.iter_mut().enumerate() {
                    *is_changed = *is_changed || new[column * BLOCK..][..BLOCK] != old[column * BLOCK..][..BLOCK];
                }
            }
            for column in (0..across).filter(|&column| self.changes[column]) {
                let left = column * BLOCK;
                for line in lines.clone() {
                    let (new, old) = (&pixels[line * stride + left..][..BLOCK], line * self.width + left);
                    self.last[old..][..BLOCK].copy_from_slice(new);
                }
                self.record.extend_from_slice(&((band * across + column) as u16).to_le_bytes());
                let at = self.record.len();
                self.record.resize(at + BLOCK_BYTES, 0);
                let out = (&mut self.record[at..]).try_into().expect("a block's room is its bytes");
                block(&pixels[band * BLOCK * stride + column * BLOCK..], stride, self.format, out);
                count += 1;
            }
        }
        self.is_whole = false;
        let length = (self.record.len() - HEAD_BYTES) as u32;
        self.record[LENGTH_AT..][..size_of::<u32>()].copy_from_slice(&length.to_le_bytes());
        self.record[HEAD_BYTES + COUNT_AT..][..size_of::<u16>()].copy_from_slice(&count.to_le_bytes());
        Some(&self.record)
    }

    /// The record that ends a recording at `time_us`.
    pub fn end(&mut self, time_us: u64, skips: u32) -> &[u8] {
        self.record.clear();
        Head { kind: Kind::End, time_us, length: 0, skips }.write(&mut self.record);
        &self.record
    }
}

fn block(pixels: &[u32], stride: usize, format: PixelFormat, out: &mut [u8; BLOCK_BYTES]) {
    let (lumas, differences) = out.split_at_mut(BLOCK * BLOCK);
    let (blues, reds) = differences.split_at_mut(CHROMA * CHROMA);
    let (lumas, blues, reds) =
        (lumas.as_chunks_mut::<BLOCK>().0, blues.as_chunks_mut::<CHROMA>().0, reds.as_chunks_mut::<CHROMA>().0);
    // Whole lines of whole channels, which the compiler vectorizes.
    let channel = |line: &[u32; BLOCK], shift: u8| -> [i32; BLOCK] {
        core::array::from_fn(|at| (line[at] >> shift & 0xFF) as i32)
    };
    for pair in 0..CHROMA {
        let mut sums = [[0i32; CHROMA]; 3];
        for half in 0..2 {
            let line: &[u32; BLOCK] =
                pixels[(2 * pair + half) * stride..][..BLOCK].try_into().expect("a line of a block is its width");
            let (r, g, b) =
                (channel(line, format.r_shift), channel(line, format.g_shift), channel(line, format.b_shift));
            lumas[2 * pair + half] = core::array::from_fn(|at| sample(LUMA_FLOOR, BT709.luma, [r[at], g[at], b[at]]));
            for (sum, channel) in sums.iter_mut().zip([r, g, b]) {
                for (column, sum) in sum.iter_mut().enumerate() {
                    *sum += channel[2 * column] + channel[2 * column + 1];
                }
            }
        }
        let [r, g, b] = sums.map(|sum| sum.map(|four| (four + 2) >> 2));
        let mean = |column: usize| [r[column], g[column], b[column]];
        blues[pair] = core::array::from_fn(|column| sample(CHROMA_MIDDLE, BT709.blue, mean(column)));
        reds[pair] = core::array::from_fn(|column| sample(CHROMA_MIDDLE, BT709.red, mean(column)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::support::{SIDE, frame, records};
    use super::super::{Decoder, ENTRY_BYTES, FRAME_HEAD_BYTES, StartBody, frame_size};
    use super::*;
    use crate::gfx::support::RGBX;

    fn samples(rgb: [i32; 3]) -> (u8, u8, u8) {
        (
            sample(LUMA_FLOOR, BT709.luma, rgb),
            sample(CHROMA_MIDDLE, BT709.blue, rgb),
            sample(CHROMA_MIDDLE, BT709.red, rgb),
        )
    }

    #[test]
    fn bt_709_limited_range() {
        assert_eq!(samples([255, 255, 255]), (235, 128, 128), "white");
        assert_eq!(samples([0, 0, 0]), (16, 128, 128), "black");
        assert_eq!(samples([255, 0, 0]), (63, 102, 240), "red");
        assert_eq!(samples([0, 255, 0]), (173, 42, 26), "green");
        assert_eq!(samples([0, 0, 255]), (32, 240, 118), "blue");
    }

    #[test]
    fn first_frame_whole_then_changes() {
        let mut encoder = Encoder::new(SIDE, SIDE, RGBX).unwrap();
        let red = frame(RGBX, |_, _| (255, 0, 0));
        let first = records(encoder.frame(&red, SIDE, 1_000, 0).unwrap());
        assert_eq!(first.len(), 1);
        let (head, body) = &first[0];
        assert_eq!(
            (*head, frame_size(body), body[4]),
            (Head { kind: Kind::Frame, time_us: 1_000, length: body.len() as u32, skips: 0 }, Some((SIDE, SIDE)), 4),
            "all four blocks of the first"
        );
        let mut decoder = Decoder::new(SIDE, SIDE).unwrap();
        decoder.apply(body).unwrap();
        let lumas = SIDE * SIDE;
        assert!(decoder.planes()[..lumas].iter().all(|&y| y == 63), "red everywhere");
        assert!(decoder.planes()[lumas..lumas + lumas / 4].iter().all(|&blue| blue == 102));
        assert!(decoder.planes()[lumas + lumas / 4..].iter().all(|&red| red == 240));
        let dotted = frame(RGBX, |x, y| if (x, y) == (20, 3) { (255, 255, 255) } else { (255, 0, 0) });
        let second = records(encoder.frame(&dotted, SIDE, 17_667, 2).unwrap());
        let (head, body) = &second[0];
        assert_eq!(
            (head.skips, body[4], u16::from_le_bytes([body[8], body[9]])),
            (2, 1, 1),
            "one block changed, the top right one, after two frames that were not recorded"
        );
        decoder.apply(body).unwrap();
        assert_eq!(
            (decoder.planes()[3 * SIDE + 20], decoder.planes()[3 * SIDE + 19], decoder.planes()[0]),
            (235, 63, 63),
            "the white pixel stands in the red"
        );
        let unchanged = records(encoder.frame(&dotted, SIDE, 34_333, 0).unwrap());
        assert_eq!(unchanged[0].1[4], 0, "a frame that changed nothing sends no block");
        let lower =
            frame(RGBX, |x, y| if [(20, 3), (5, 20)].contains(&(x, y)) { (255, 255, 255) } else { (255, 0, 0) });
        let below = records(encoder.frame(&lower, SIDE, 40_000, 0).unwrap());
        let (head, body) = &below[0];
        assert_eq!(
            (body[4], u16::from_le_bytes([body[8], body[9]]), head.length as usize, body.len()),
            (1, 2, FRAME_HEAD_BYTES + ENTRY_BYTES, FRAME_HEAD_BYTES + ENTRY_BYTES),
            "one block of the second band changed, the bottom left one, and the head tells its length"
        );
        decoder.apply(body).unwrap();
        assert_eq!((decoder.planes()[20 * SIDE + 5], decoder.planes()[3 * SIDE + 20]), (235, 235));
        let start = records(encoder.start(50_000));
        assert_eq!(
            (start[0].0.kind, start[0].0.time_us, StartBody::read(&start[0].1)),
            (Kind::Start, 50_000, Some(StartBody { width: SIDE, height: SIDE, rate: 60 })),
            "a start tells the size of the frames and the screen's rate"
        );
        let again = records(encoder.frame(&dotted, SIDE, 51_000, 0).unwrap());
        assert_eq!(again[0].1[4], 4, "after a start a frame goes whole again");
        let end = records(encoder.end(60_000, 1));
        assert_eq!(end, [(Head { kind: Kind::End, time_us: 60_000, length: 0, skips: 1 }, Vec::new())]);
    }

    #[test]
    fn channels_and_stride() {
        let stride = SIDE + 7;
        for format in [RGBX, PixelFormat::XRGB8888] {
            let narrow = frame(format, |x, _| if x < BLOCK { (0, 255, 0) } else { (0, 0, 255) });
            let mut padded = vec![0u32; stride * SIDE];
            for line in 0..SIDE {
                padded[line * stride..][..SIDE].copy_from_slice(&narrow[line * SIDE..][..SIDE]);
            }
            let mut encoder = Encoder::new(SIDE, SIDE, format).unwrap();
            let mut decoder = Decoder::new(SIDE, SIDE).unwrap();
            decoder.apply(&records(encoder.frame(&padded, stride, 0, 0).unwrap())[0].1).unwrap();
            let lumas = SIDE * SIDE;
            assert_eq!(
                (
                    decoder.planes()[0],
                    decoder.planes()[SIDE - 1],
                    decoder.planes()[lumas],
                    decoder.planes()[lumas + 15]
                ),
                (173, 32, 42, 240),
                "{format:?}: green on the left, blue on the right"
            );
        }
        let paired = frame(RGBX, |x, y| match (x % 2, y % 4) {
            (0, 0 | 1) | (1, 2) => (255, 0, 0),
            (_, 3) => (255, 255, 255),
            _ => (0, 0, 0),
        });
        let mut decoder = Decoder::new(SIDE, SIDE).unwrap();
        decoder
            .apply(&records(Encoder::new(SIDE, SIDE, RGBX).unwrap().frame(&paired, SIDE, 0, 0).unwrap())[0].1)
            .unwrap();
        let (lumas, half) = (SIDE * SIDE, SIDE / 2);
        let (blues, reds) = (&decoder.planes()[lumas..lumas + lumas / 4], &decoder.planes()[lumas + lumas / 4..]);
        assert_eq!(
            (blues[0], reds[0], blues[7], reds[half + 3]),
            (samples([128, 0, 0]).1, samples([128, 0, 0]).2, samples([128, 0, 0]).1, samples([191, 128, 128]).2),
            "each difference is of the mean of its four pixels, rounded"
        );
        let mut encoder = Encoder::new(SIDE, SIDE, RGBX).unwrap();
        assert!(encoder.frame(&[0; 10], SIDE, 0, 0).is_none(), "too few pixels for the size");
        assert!(encoder.frame(&[0; SIDE * SIDE], SIDE - 1, 0, 0).is_none(), "lines shorter than the frame");
        assert!(
            Encoder::new(SIDE + 1, SIDE, RGBX).is_none()
                && Encoder::new(0, SIDE, RGBX).is_none()
                && Decoder::new(SIDE, SIDE - 1).is_none(),
            "only frames of whole blocks"
        );
    }

    #[test]
    fn encodes_without_allocating() {
        let mut encoder = Encoder::new(SIDE, SIDE, RGBX).unwrap();
        let pixels = frame(RGBX, |x, y| ((x * 8) as u8, (y * 8) as u8, 99));
        let before = crate::base::alloc_count::allocations();
        encoder.start(0);
        encoder.frame(&pixels, SIDE, 0, 0);
        encoder.frame(&pixels, SIDE, 1, 0);
        encoder.end(2, 0);
        assert_eq!(crate::base::alloc_count::allocations(), before);
    }
}
