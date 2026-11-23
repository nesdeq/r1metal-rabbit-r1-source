use super::Surface;
use crate::base::sampling::{middle, straddle};
use crate::gfx::Rect;
use alloc::vec::Vec;

const MIX_BITS: u32 = 2;
const MIX_ONE: u32 = 1 << MIX_BITS;

#[derive(Default)]
pub(crate) struct Stretching {
    columns: Vec<(usize, usize, u32)>,
    near: Vec<u32>,
    far: Vec<u32>,
    line: Vec<u32>,
}

#[inline(always)]
const fn mean(one: u32, other: u32) -> u32 {
    (one & other) + (((one ^ other) & 0xFEFE_FEFE) >> 1)
}

#[inline(always)]
const fn mix(from: u32, to: u32, toward: u32) -> u32 {
    let half = mean(from, to);
    // Two choices without a jump, which a place's quarter gives the processor no way to foresee.
    let (start, end) = if toward & 2 == 0 { (from, half) } else { (half, to) };
    if toward & 1 == 0 { start } else { mean(start, end) }
}

fn span_of(pixel: i32, pixels: i32, samples: usize) -> (usize, usize) {
    let from = pixel as usize * samples / pixels as usize;
    (from, ((pixel as usize + 1) * samples / pixels as usize).clamp(from + 1, samples))
}

impl Surface<'_> {
    pub(crate) fn stretch(
        &mut self,
        stretching: &mut Stretching,
        area: Rect,
        within: Rect,
        width: usize,
        words: &[u32],
    ) {
        let height = words.len().checked_div(width).unwrap_or(0);
        let Some(visible) = self.to_visible(area, within).filter(|_| height > 0) else {
            return;
        };
        let Stretching { columns, near, far, line } = stretching;
        let place = |pixel: i32, pixels: i32, samples: usize| {
            straddle(middle(pixel as usize, pixels as usize, (samples, 1), i64::from(MIX_ONE)), samples, MIX_BITS)
        };
        columns.clear();
        columns.extend((visible.x..visible.right()).map(|x| place(x - area.x, area.w, width)));
        let columns = &columns[..];
        let across = |from: &[u32], to: &mut [u32]| {
            for (pixel, &(before, after, toward)) in to.iter_mut().zip(columns) {
                *pixel = mix(from[before], from[after], toward);
            }
        };
        // One loop for each share of the way down, so each is a plain run over whole lines.
        let down = |upper: &[u32], lower: &[u32], toward: u32, to: &mut [u32]| {
            let lines = to.iter_mut().zip(upper).zip(lower);
            match toward {
                0 => to.copy_from_slice(upper),
                1 => lines.for_each(|((pixel, &upper), &lower)| *pixel = mean(upper, mean(upper, lower))),
                2 => lines.for_each(|((pixel, &upper), &lower)| *pixel = mean(upper, lower)),
                _ => lines.for_each(|((pixel, &upper), &lower)| *pixel = mean(mean(upper, lower), lower)),
            }
        };
        if area.h >= height as i32 {
            // A picture made taller has each line under several of the area's: a line is stretched once.
            for stretched in [&mut *near, &mut *far] {
                stretched.clear();
                stretched.resize(columns.len(), 0);
            }
            let (mut near, mut far) = (&mut near[..], &mut far[..]);
            let (mut near_line, mut far_line) = (usize::MAX, usize::MAX);
            for y in visible.y..visible.bottom() {
                let (upper, lower, toward) = place(y - area.y, area.h, height);
                if upper == far_line {
                    core::mem::swap(&mut near, &mut far);
                    core::mem::swap(&mut near_line, &mut far_line);
                }
                if upper != near_line {
                    across(&words[upper * width..][..width], near);
                    near_line = upper;
                }
                if toward != 0 && lower != far_line {
                    across(&words[lower * width..][..width], far);
                    far_line = lower;
                }
                down(
                    near,
                    far,
                    toward,
                    &mut self.pixels[y as usize * self.stride + visible.x as usize..][..columns.len()],
                );
            }
        } else {
            // A picture made shorter gives two lines to each of the area's: they are mixed first, and stretched once.
            let first = columns.first().map_or(0, |column| column.0);
            line.clear();
            line.resize(columns.last().map_or(0, |column| column.1 + 1 - first), 0);
            for y in visible.y..visible.bottom() {
                let (upper, lower, toward) = place(y - area.y, area.h, height);
                down(
                    &words[upper * width + first..][..line.len()],
                    &words[lower * width + first..][..line.len()],
                    toward,
                    line,
                );
                let row = &mut self.pixels[y as usize * self.stride + visible.x as usize..][..columns.len()];
                for (pixel, &(before, after, toward)) in row.iter_mut().zip(columns) {
                    *pixel = mix(line[before - first], line[after - first], toward);
                }
            }
        }
    }

    /// Draws a smaller picture over `area`, each pixel the bytewise mean of those it covers.
    pub fn shrink(&mut self, area: Rect, width: usize, words: &[u32]) {
        let height = words.len().checked_div(width).unwrap_or(0);
        let Some(visible) = area.clip(self.bounds()).filter(|_| height > 0) else {
            return;
        };
        // A pixel's columns begin where the last ended, so a row goes along without a division.
        let (across, first) = (area.w as usize, (visible.x - area.x) as usize);
        let (step, part) = (width / across, width % across);
        for y in visible.y..visible.bottom() {
            let (top, bottom) = span_of(y - area.y, area.h, height);
            let (mut left, mut over) = (first * width / across, first * width % across);
            for x in visible.x..visible.right() {
                let (mut next, mut past) = (left + step, over + part);
                if past >= across {
                    (next, past) = (next + 1, past - across);
                }
                let right = next.clamp(left + 1, width);
                let (mut sums, mut count) = ([0u32; 4], 0u32);
                for row in top..bottom {
                    for word in &words[row * width + left..row * width + right] {
                        sums.iter_mut().enumerate().for_each(|(at, sum)| *sum += word >> (8 * at) & 0xFF);
                        count += 1;
                    }
                }
                let averaged =
                    sums.iter().enumerate().fold(0, |word, (at, sum)| word | (sum / count.max(1)) << (8 * at));
                self.pixels[y as usize * self.stride + x as usize] = averaged;
                (left, over) = (next, past);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::support::{RGBX, speckle};
    use alloc::vec;

    #[test]
    fn stretch_mixes_neighbours() {
        let (dark, light) = (0x0020_4060, 0x00A0_C0E0);
        let (picture, mut stretching) = ([dark, light, dark, light], Stretching::default());
        let mut memory = vec![0u32; 8 * 4];
        let mut surface = Surface::new(&mut memory, 8, 4, 8, RGBX).unwrap();
        surface.stretch(&mut stretching, Rect::new(0, 0, 2, 2), surface.bounds(), 2, &picture);
        assert_eq!(
            (memory[0], memory[1], memory[8], memory[9]),
            (dark, light, dark, light),
            "at its own size it is a copy"
        );
        let mut surface = Surface::new(&mut memory, 8, 4, 8, RGBX).unwrap();
        surface.stretch(&mut stretching, Rect::new(0, 0, 8, 4), Rect::new(0, 0, 8, 3), 2, &picture);
        assert_eq!(
            (memory[0], memory[1], memory[6], memory[7]),
            (dark, dark, light, light),
            "past the middle of the outermost pixels there is nothing to mix with"
        );
        assert_eq!(
            (memory[3], memory[4], memory[5]),
            (0x0040_6080, 0x0060_80A0, 0x0080_A0C0),
            "a quarter, a half and three quarters of the way from one to the other, each channel by itself"
        );
        assert!(
            memory[2] < memory[3] && memory[4] < memory[5] && memory[..8] == memory[16..24],
            "lighter from left to right, every line alike"
        );
        assert_eq!(memory[24..], [0u32; 8], "and nothing outside the area it was given");
        let mut surface = Surface::new(&mut memory, 8, 4, 8, RGBX).unwrap();
        surface.stretch(&mut stretching, Rect::new(-4, -2, 4, 4), surface.bounds(), 2, &picture);
        surface.stretch(&mut stretching, Rect::new(0, 0, 4, 4), surface.bounds(), 0, &picture);
        surface.stretch(&mut stretching, Rect::new(0, 0, 4, 4), surface.bounds(), 2, &[]);
        let corner = [0x0000_0000, 0x0040_4040, 0x0080_8080, 0x00C0_C0C0];
        let large: [u32; 16] =
            core::array::from_fn(|at| if at % 4 < 2 && at / 4 < 2 { corner[at / 4 * 2 + at % 4] } else { 0x00FF_FFFF });
        surface.stretch(&mut stretching, Rect::new(4, 0, 2, 2), surface.bounds(), 4, &large);
        surface.stretch(&mut stretching, Rect::new(-1, 2, 2, 2), surface.bounds(), 4, &large);
        assert_eq!(
            (memory[4], memory[5], memory[12], memory[13]),
            (0x0060_6060, 0x00FF_FFFF, 0x00FF_FFFF, 0x00FF_FFFF),
            "made smaller, each pixel is mixed from the four it lies between"
        );
        assert_eq!(
            (memory[16], memory[24]),
            (0x00FF_FFFF, 0x00FF_FFFF),
            "and one that hangs over the edge shows what of it is inside"
        );
        assert_eq!((mix(0x00FF_FFFF, 0, 2), mix(dark, light, 0), mean(light, light)), (0x007F_7F7F, dark, light));
    }

    #[test]
    fn shrink_means_covered_pixels() {
        let picture: [u32; 16] = [
            0x0000_0000,
            0x0004_0800,
            0x0010_0000,
            0x0010_0000, //
            0x0008_0400,
            0x0004_0400,
            0x0010_0000,
            0x0010_0004, //
            0x00FF_FFFF,
            0x00FF_FFFF,
            0x0000_0000,
            0x0000_0000, //
            0x00FF_FFFF,
            0x00FF_FFFF,
            0x0000_0000,
            0x00FF_0000, //
        ];
        let mut memory = vec![0x0012_3456u32; 3 * 3];
        let mut surface = Surface::new(&mut memory, 3, 3, 3, RGBX).unwrap();
        surface.shrink(Rect::new(0, 0, 2, 2), 4, &picture);
        assert_eq!(
            [memory[0], memory[1], memory[3], memory[4]],
            [0x0004_0400, 0x0010_0001, 0x00FF_FFFF, 0x003F_0000],
            "each pixel the mean of the two by two it covers, each byte by itself"
        );
        assert_eq!((memory[2], memory[8]), (0x0012_3456, 0x0012_3456), "and nothing outside the area");
        let mut surface = Surface::new(&mut memory, 3, 3, 3, RGBX).unwrap();
        surface.shrink(Rect::new(-1, 1, 2, 2), 4, &picture);
        surface.shrink(Rect::new(0, 0, 2, 2), 0, &picture);
        assert_eq!(
            (memory[3], memory[6]),
            (0x0010_0001, 0x003F_0000),
            "an area that hangs over the edge draws what of it lies on the surface, and a picture of no width nothing"
        );
    }

    #[test]
    fn shrink_spans_by_division() {
        let (width, height) = (53usize, 41usize);
        let picture: Vec<u32> = (0..(width * height) as u32).map(speckle).collect();
        let areas = [
            (Rect::new(0, 0, 17, 13), 20),
            (Rect::new(-5, 3, 53, 41), 60),
            (Rect::new(2, -1, 7, 60), 12),
            (Rect::new(0, 0, 90, 9), 96),
        ];
        for (area, side) in areas {
            let mut memory = vec![0u32; side * side];
            Surface::new(&mut memory, side as u32, side as u32, side, RGBX).unwrap().shrink(area, width, &picture);
            for (at, &word) in memory.iter().enumerate() {
                let (x, y) = ((at % side) as i32, (at / side) as i32);
                let expected = if area.contains(x, y) {
                    let ((top, bottom), (left, right)) =
                        (span_of(y - area.y, area.h, height), span_of(x - area.x, area.w, width));
                    let block: Vec<u32> = (top..bottom)
                        .flat_map(|row| picture[row * width + left..row * width + right].to_vec())
                        .collect();
                    (0..4).fold(0, |mean, byte| {
                        let sum: u32 = block.iter().map(|word| word >> (8 * byte) & 0xFF).sum();
                        mean | (sum / block.len() as u32) << (8 * byte)
                    })
                } else {
                    0
                };
                assert_eq!(word, expected, "({x}, {y}) of {area:?}");
            }
        }
    }
}
