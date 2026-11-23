mod resample;
mod text;

pub(crate) use resample::Stretching;
pub(crate) use text::Lettering;

use super::Rect;
use super::mask::{Mask, SHADES, Strip, each_run};
use super::stamp::Stamp;
use crate::base::pixel::{Color, PixelFormat};
use core::ops::Range;

const HALVES: u32 = 0x00FF_00FF;
const WEIGHT_BITS: u32 = 8;
pub(super) const WEIGHT_WHOLE: u32 = 1 << WEIGHT_BITS;
pub(super) const WHITE: Color = Color(0xFF_FFFF);
pub(super) const BLACK: Color = Color(0);

const fn halves(word: u32) -> (u32, u32) {
    (word & HALVES, word >> u8::BITS & HALVES)
}

#[inline(always)]
fn toward_halves(under: u32, (low, high): (u32, u32), weight: u32) -> u32 {
    let mix = |under: u32, over: u32| ((under * (WEIGHT_WHOLE - weight) + over * weight) >> WEIGHT_BITS) & HALVES;
    let (under_low, under_high) = halves(under);
    mix(under_low, low) | mix(under_high, high) << u8::BITS
}

/// A drawing target over borrowed pixels, usually a mapped framebuffer page.
pub struct Surface<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    stride: usize,
    format: PixelFormat,
}

impl<'a> Surface<'a> {
    /// A target of `stride` pixels per row; none if `pixels` is too short.
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32, stride: usize, format: PixelFormat) -> Option<Self> {
        let (w, h) = (width as usize, height as usize);
        let needed = if h == 0 { 0 } else { stride.checked_mul(h - 1)?.checked_add(w)? };
        (stride >= w && pixels.len() >= needed).then_some(Surface {
            pixels,
            width: width as i32,
            height: height as i32,
            stride,
            format,
        })
    }

    pub(crate) const fn width(&self) -> i32 {
        self.width
    }

    pub(crate) const fn height(&self) -> i32 {
        self.height
    }

    pub(super) const fn format(&self) -> PixelFormat {
        self.format
    }

    /// The whole of it, from its top left corner.
    pub const fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub(super) fn to_visible(&self, area: Rect, within: Rect) -> Option<Rect> {
        area.clip(within)?.clip(self.bounds())
    }

    #[cfg(test)]
    pub(super) fn pixel(&self, x: i32, y: i32) -> Option<Color> {
        self.bounds().contains(x, y).then(|| self.format.unpack(self.pixels[y as usize * self.stride + x as usize]))
    }

    pub(crate) fn fill(&mut self, rect: Rect, color: Color) {
        let Some(visible) = rect.clip(self.bounds()) else {
            return;
        };
        let word = self.format.pack(color);
        for y in visible.y..visible.bottom() {
            let row = y as usize * self.stride;
            self.pixels[row + visible.x as usize..row + visible.right() as usize].fill(word);
        }
    }

    /// Copies what lies within `within` of a picture of `width` words a line, its top left corner at (x, y).
    pub fn blit(&mut self, within: Rect, x: i32, y: i32, width: usize, words: &[u32]) {
        if width == 0 {
            return;
        }
        let rect = Rect::new(x, y, width as i32, (words.len() / width) as i32);
        let Some(visible) = self.to_visible(rect, within) else {
            return;
        };
        for row in visible.y..visible.bottom() {
            let from = (row - y) as usize * width + (visible.x - x) as usize;
            let to = row as usize * self.stride + visible.x as usize;
            self.pixels[to..to + visible.w as usize].copy_from_slice(&words[from..from + visible.w as usize]);
        }
    }

    pub(super) fn shades(&self, ink: Color, ground: Color) -> [u32; SHADES] {
        let last = (SHADES - 1) as u32;
        core::array::from_fn(|step| self.format.pack(ground.toward(ink, step as u32, last)))
    }

    pub(super) fn paint(&mut self, strips: &[Strip], within: Rect, shades: &[u32; SHADES]) {
        let Some(visible) = within.clip(self.bounds()) else {
            return;
        };
        for (pixels, shade) in visible_strips(strips, visible, self.stride) {
            self.pixels[pixels].fill(shades[usize::from(shade)]);
        }
    }

    pub(super) fn paint_over(&mut self, strips: &[Strip], within: Rect, ink: Color) {
        let Some(visible) = within.clip(self.bounds()) else {
            return;
        };
        let over = halves(self.format.pack(ink));
        for (pixels, shade) in visible_strips(strips, visible, self.stride) {
            // A shade of 255ths is weighed in 256ths, a whole shade as the whole.
            let shade = u32::from(shade);
            let weight = shade + (shade >> 7);
            for pixel in &mut self.pixels[pixels] {
                *pixel = toward_halves(*pixel, over, weight);
            }
        }
    }

    pub(super) fn stamp(&mut self, stamp: &Stamp, within: Rect, x: i32, y: i32, colour: Color) {
        let side = stamp.side as i32;
        let Some(visible) = self.to_visible(Rect::new(x, y, side, side), within) else {
            return;
        };
        let (over, from) = (halves(self.format.pack(colour)), (visible.x - x) as usize);
        for row in visible.y..visible.bottom() {
            let (light, dark) = stamp.row((row - y) as usize);
            let line = &mut self.pixels[row as usize * self.stride..][visible.x as usize..visible.right() as usize];
            for ((pixel, &light), &dark) in line.iter_mut().zip(&light[from..]).zip(&dark[from..]) {
                if dark == 0 {
                    continue;
                }
                let darkened = toward_halves(*pixel, (0, 0), u32::from(dark));
                *pixel = toward_halves(darkened, over, u32::from(light));
            }
        }
    }

    /// Fills `mask` and paints it at (x, y): `ink` over `ground` by coverage, nothing at zero.
    pub fn cover(&mut self, x: i32, y: i32, mask: &mut Mask, ink: Color, ground: Color) {
        mask.fill();
        let (shades, width) = (self.shades(ink, ground), self.width);
        for row in mask.rows() {
            let line_y = y + row as i32;
            if line_y < 0 || line_y >= self.height {
                continue;
            }
            let line = &mut self.pixels[line_y as usize * self.stride..][..width as usize];
            let (first, changes) = mask.changes(row);
            // Coverage changes only where a cell holds a change: each run between two is painted at once.
            each_run(first, changes, |from, to, shade| {
                let (left, right) = (x + from as i32, x + to as i32);
                if shade > 0 && right > 0 && left < width {
                    line[left.max(0) as usize..right.min(width) as usize].fill(shades[usize::from(shade)]);
                }
            });
        }
    }
}

fn visible_strips(strips: &[Strip], visible: Rect, stride: usize) -> impl Iterator<Item = (Range<usize>, u8)> + '_ {
    let first = strips.partition_point(|strip| i32::from(strip.row) < visible.y);
    let rows = strips[first..].iter().take_while(move |strip| i32::from(strip.row) < visible.bottom());
    rows.filter_map(move |strip| {
        let (left, right) = (i32::from(strip.left).max(visible.x), i32::from(strip.right).min(visible.right()));
        let line = i32::from(strip.row) as usize * stride;
        (left < right).then(|| (line + left as usize..line + right as usize, strip.shade))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::icon;
    use crate::gfx::support::{RGBX, SQUARE};
    use alloc::vec;

    #[test]
    fn fill_clips_and_honours_stride() {
        let mut memory = vec![0u32; 6 * 4];
        let mut surface = Surface::new(&mut memory, 4, 4, 6, RGBX).unwrap();
        surface.fill(Rect::new(2, 2, 10, 10), WHITE);
        surface.fill(Rect::new(-5, -5, 2, 2), WHITE);
        assert_eq!(surface.pixel(2, 2), Some(WHITE));
        assert_eq!(surface.pixel(1, 2), Some(BLACK));
        assert_eq!(surface.pixel(4, 2), None);
        assert_eq!(memory.iter().filter(|&&pixel| pixel != 0).count(), 4);
        assert_eq!(memory[2 * 6 + 4], 0);
    }

    #[test]
    fn blit_clips() {
        let picture: [u32; 6] = [1, 2, 3, 4, 5, 6];
        let mut memory = vec![0u32; 6 * 4];
        let mut surface = Surface::new(&mut memory, 4, 4, 6, RGBX).unwrap();
        let whole = surface.bounds();
        surface.blit(whole, 1, 1, 3, &picture);
        surface.blit(whole, -2, 3, 3, &picture);
        surface.blit(whole, 3, -1, 3, &picture);
        surface.blit(whole, 9, 9, 3, &picture);
        surface.blit(whole, 0, 0, 0, &picture);
        #[rustfmt::skip]
        let expected = [
            0, 0, 0, 4, 0, 0,
            0, 1, 2, 3, 0, 0,
            0, 4, 5, 6, 0, 0,
            3, 0, 0, 0, 0, 0,
        ];
        assert_eq!(memory, expected);
    }

    #[test]
    fn blit_within_area() {
        let picture: [u32; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let mut memory = vec![0u32; 5 * 5];
        let mut surface = Surface::new(&mut memory, 5, 5, 5, RGBX).unwrap();
        surface.blit(Rect::new(2, 1, 2, 2), 1, 1, 3, &picture);
        surface.blit(Rect::new(4, 4, 9, 9), 3, 3, 3, &picture);
        surface.blit(Rect::new(0, 0, 0, 5), 0, 0, 3, &picture);
        #[rustfmt::skip]
        let expected = [
            0, 0, 0, 0, 0,
            0, 0, 2, 3, 0,
            0, 0, 5, 6, 0,
            0, 0, 0, 0, 0,
            0, 0, 0, 0, 5,
        ];
        assert_eq!(memory, expected);
    }

    #[test]
    fn refuses_short_buffers() {
        assert!(Surface::new(&mut [0u32; 15], 4, 4, 4, RGBX).is_none());
        assert!(Surface::new(&mut [0u32; 16], 4, 4, 3, RGBX).is_none());
        assert!(Surface::new(&mut [0u32; 16], 4, 4, 4, RGBX).is_some());
    }

    #[test]
    fn cover_by_coverage_and_clip() {
        let mut mask = Mask::new(4, 2);
        mask.outline([(0.5, 0.0), (0.5, 2.0), (2.0, 2.0), (2.0, 0.0)].into_iter());
        let grey = Color(0x40_4040);
        let mut memory = vec![RGBX.pack(grey); 6 * 4];
        let mut surface = Surface::new(&mut memory, 6, 4, 6, RGBX).unwrap();
        surface.cover(1, 1, &mut mask, WHITE, BLACK);
        assert_eq!(
            [(1, 1), (2, 1), (3, 1), (2, 0), (2, 2), (2, 3)].map(|(x, y)| surface.pixel(x, y)),
            [Color(0x80_8080), WHITE, grey, grey, WHITE, grey].map(Some)
        );
        let mut mask = Mask::new(8, 8);
        mask.outline([(0.0, 0.0), (0.0, 8.0), (8.0, 8.0), (8.0, 0.0)].into_iter());
        let mut memory = vec![0u32; 4 * 4];
        let mut surface = Surface::new(&mut memory, 4, 4, 4, RGBX).unwrap();
        surface.cover(-6, -6, &mut mask, WHITE, BLACK);
        surface.cover(3, 3, &mut mask, WHITE, BLACK);
        assert_eq!(
            [0, 1, 2, 3].map(|at| surface.pixel(at, at)),
            [WHITE, WHITE, BLACK, WHITE].map(Some),
            "of masks that hang over the edges what lies on the surface"
        );
    }

    #[test]
    fn paint_over_mixes_by_shade() {
        let (grey, amber) = (Color(0x40_6080), Color(0xFF_B000));
        for format in [RGBX, PixelFormat::XRGB8888] {
            let mut memory = vec![format.pack(grey); 6 * 3];
            let mut surface = Surface::new(&mut memory, 6, 3, 6, format).unwrap();
            let strips = [
                Strip { row: 1, left: 0, right: 2, shade: 255 },
                Strip { row: 1, left: 2, right: 4, shade: 128 },
                Strip { row: 1, left: 4, right: 6, shade: 0 },
            ];
            surface.paint_over(&strips, Rect::new(1, 0, 6, 3), amber);
            let half = |under: u8, over: u8| ((u32::from(under) * 127 + u32::from(over) * 129) >> 8) as u8;
            let between = Color(
                u32::from(half(grey.r(), amber.r())) << 16
                    | u32::from(half(grey.g(), amber.g())) << 8
                    | u32::from(half(grey.b(), amber.b())),
            );
            assert_eq!(
                (
                    surface.pixel(0, 1),
                    surface.pixel(1, 1),
                    surface.pixel(2, 1),
                    surface.pixel(4, 1),
                    surface.pixel(1, 0)
                ),
                (Some(grey), Some(amber), Some(between), Some(grey), Some(grey)),
                "whole over the picture where it covers all, half way where it covers half, nothing where none, and nothing outside what it is given: {format:?}"
            );
        }
    }

    #[test]
    fn stamp_rim_and_icon() {
        let stamp = Stamp::new(&icon::Icon::from_svg(SQUARE).unwrap(), 10.0, 3);
        let amber = Color(0xDF_9A00);
        for (format, under) in [(RGBX, WHITE), (PixelFormat::XRGB8888, Color(0x20_4060)), (RGBX, BLACK)] {
            let mut memory = vec![format.pack(under); 24 * 24];
            let mut surface = Surface::new(&mut memory, 24, 24, 24, format).unwrap();
            surface.stamp(&stamp, surface.bounds(), 4, 4, amber);
            assert_eq!(
                (
                    surface.pixel(12, 12),
                    surface.pixel(6, 12),
                    surface.pixel(4, 4),
                    surface.pixel(5, 12),
                    surface.pixel(20, 12)
                ),
                (Some(amber), Some(BLACK), Some(under), Some(under), Some(under)),
                "the icon in its colour, black about it, and the picture as it was beyond the rim: over {under:?}"
            );
        }
        let mut memory = vec![RGBX.pack(WHITE); 24 * 24];
        let mut surface = Surface::new(&mut memory, 24, 24, 24, RGBX).unwrap();
        surface.stamp(&stamp, Rect::new(0, 0, 12, 24), 4, 4, amber);
        assert_eq!(
            (surface.pixel(11, 12), surface.pixel(12, 12)),
            (Some(amber), Some(WHITE)),
            "only what lies inside the area it is given"
        );
        surface.stamp(&stamp, surface.bounds(), -10, 14, amber);
        surface.stamp(&stamp, surface.bounds(), 40, 40, amber);
        assert_eq!(
            (surface.pixel(2, 17), surface.pixel(0, 20)),
            (Some(BLACK), Some(amber)),
            "one that hangs over the edge shows what of it is inside"
        );
    }
}
