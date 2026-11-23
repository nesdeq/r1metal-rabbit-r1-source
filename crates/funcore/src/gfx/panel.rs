//! The panel's pages as its driver reports them, and the ring they rotate in.

use crate::base::pixel::PixelFormat;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const BITS_PER_PIXEL: u32 = 32;
const BITS_PER_CHANNEL: u32 = 8;
const BYTES_PER_PIXEL: u32 = 4;

/// The driver's report of the panel; `channels` holds each colour's first bit and bit count.
#[derive(Clone, Copy, Debug)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub lines: u32,
    pub bits_per_pixel: u32,
    pub channels: [(u32, u32); 3],
    pub line_bytes: u32,
    pub memory: u32,
}

/// Why a panel's pages cannot be drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
    Layout,
    Lines,
    NoPage,
}

impl Unsupported {
    /// Error text of a refused panel.
    pub fn to_text(self, info: &ScreenInfo) -> String {
        format!("{self:?}: {info:?}")
    }
}

/// The pages in the mapping, stacked at the screen's height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// Words per line.
    pub stride: usize,
    pub pages: u32,
    pub format: PixelFormat,
}

impl Geometry {
    /// The pages of a report; refuses all but 32-bit pixels of 8-bit channels.
    pub fn from_info(info: &ScreenInfo) -> Result<Geometry, Unsupported> {
        let ScreenInfo { width, height, lines, bits_per_pixel, channels, line_bytes, memory } = *info;
        let is_unsupported =
            |&(at, bits): &(u32, u32)| bits != BITS_PER_CHANNEL || at > BITS_PER_PIXEL - BITS_PER_CHANNEL;
        if bits_per_pixel != BITS_PER_PIXEL || channels.iter().any(is_unsupported) {
            return Err(Unsupported::Layout);
        }
        let whole_pixels = line_bytes.is_multiple_of(BYTES_PER_PIXEL);
        if width == 0 || height == 0 || !whole_pixels || line_bytes / BYTES_PER_PIXEL < width {
            return Err(Unsupported::Lines);
        }
        let capacity = u64::from(memory) / (u64::from(line_bytes) * u64::from(height));
        let pages = (lines / height).min(capacity as u32);
        if pages == 0 {
            return Err(Unsupported::NoPage);
        }
        let [r_shift, g_shift, b_shift] = channels.map(|(at, _)| at as u8);
        Ok(Geometry {
            width,
            height,
            stride: (line_bytes / BYTES_PER_PIXEL) as usize,
            pages,
            format: PixelFormat { r_shift, g_shift, b_shift },
        })
    }

    /// A page's first word in the mapping, and its word count.
    pub const fn page_words(&self, page: u32) -> (usize, usize) {
        let words = self.stride * self.height as usize;
        (page as usize * words, words)
    }

    /// A page's first line.
    pub const fn page_line(&self, page: u32) -> u32 {
        page * self.height
    }

    /// Size, channels and pages as text.
    pub fn to_text(&self) -> String {
        let PixelFormat { r_shift, g_shift, b_shift } = self.format;
        format!("{}x{} r{r_shift} g{g_shift} b{b_shift} {} pages", self.width, self.height, self.pages)
    }
}

/// A panel's size and pixel format, zeros and XRGB8888 without one.
pub const fn to_shape(geometry: Option<Geometry>) -> (u32, u32, PixelFormat) {
    match geometry {
        Some(Geometry { width, height, format, .. }) => (width, height, format),
        None => (0, 0, PixelFormat::XRGB8888),
    }
}

/// The pages in rotation, with the frame each holds where known.
pub struct Ring {
    back: u32,
    front: u32,
    frames: Vec<Option<u64>>,
}

impl Ring {
    /// The ring of `geometry` with the page `offset` lines in on screen.
    pub fn new(geometry: &Geometry, offset: u32) -> Ring {
        Ring::with_pages(geometry.pages, offset / geometry.height)
    }

    pub(super) fn with_pages(pages: u32, front: u32) -> Ring {
        let front = front % pages;
        Ring { back: (front + 1) % pages, front, frames: vec![None; pages as usize] }
    }

    /// The page drawn next, off screen.
    pub const fn back(&self) -> u32 {
        self.back
    }

    #[cfg(test)]
    pub(super) const fn front(&self) -> u32 {
        self.front
    }

    /// The frame the back page holds, where known.
    pub fn back_frame(&self) -> Option<u64> {
        self.frames[self.back as usize]
    }

    /// Notes the frame the back page holds.
    pub fn set_back_frame(&mut self, frame: u64) {
        self.frames[self.back as usize] = Some(frame);
    }

    /// The panel shows the back page: the next page becomes the back.
    pub fn advance(&mut self) {
        self.front = self.back;
        self.back = (self.back + 1) % self.frames.len() as u32;
    }

    /// The back page holds `frame`, the one on screen: the next page off screen becomes the back.
    pub fn skip_back(&mut self, frame: u64) {
        self.frames[self.back as usize] = Some(frame);
        let pages = self.frames.len() as u32;
        let next = (self.back + 1) % pages;
        self.back = if next == self.front { (next + 1) % pages } else { next };
    }

    /// Clears what every page is known to hold.
    pub fn forget(&mut self) {
        self.frames.fill(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_info() -> ScreenInfo {
        ScreenInfo {
            width: 480,
            height: 640,
            lines: 1920,
            bits_per_pixel: 32,
            channels: [(0, 8), (8, 8), (16, 8)],
            line_bytes: 1920,
            memory: 1920 * 1920,
        }
    }

    #[test]
    fn three_pages_from_info() {
        let geometry = Geometry::from_info(&panel_info()).unwrap();
        assert_eq!((geometry.width, geometry.height, geometry.stride, geometry.pages), (480, 640, 480, 3));
        assert_eq!(geometry.format, PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 });
        let padded = ScreenInfo { line_bytes: 2048, memory: 2048 * 1920, ..panel_info() };
        assert_eq!(
            Geometry::from_info(&padded).map(|geometry| geometry.stride),
            Ok(512),
            "a padded line is the stride"
        );
    }

    #[test]
    fn panel_texts() {
        assert_eq!(Geometry::from_info(&panel_info()).unwrap().to_text(), "480x640 r0 g8 b16 3 pages");
        assert_eq!(
            Unsupported::Lines.to_text(&ScreenInfo { height: 0, ..panel_info() }),
            "Lines: ScreenInfo { width: 480, height: 0, lines: 1920, bits_per_pixel: 32, channels: [(0, 8), (8, 8), (16, 8)], line_bytes: 1920, memory: 3686400 }"
        );
    }

    #[test]
    fn shape_of_panel() {
        let geometry = Geometry::from_info(&panel_info()).unwrap();
        assert_eq!(to_shape(Some(geometry)), (480, 640, geometry.format));
        assert_eq!(to_shape(None), (0, 0, PixelFormat::XRGB8888));
    }

    #[test]
    fn pages_fit_memory() {
        let pages =
            |memory: u32| Geometry::from_info(&ScreenInfo { memory, ..panel_info() }).map(|geometry| geometry.pages);
        assert_eq!(
            [pages(1920 * 640 * 2 + 100), pages(1920 * 640 + 100), pages(100)],
            [Ok(2), Ok(1), Err(Unsupported::NoPage)]
        );
        assert_eq!(Geometry::from_info(&ScreenInfo { bits_per_pixel: 16, ..panel_info() }), Err(Unsupported::Layout));
        assert_eq!(
            Geometry::from_info(&ScreenInfo { channels: [(0, 5), (5, 6), (11, 5)], ..panel_info() }),
            Err(Unsupported::Layout)
        );
        let red = |at: u32| {
            let channels = [(at, 8), (8, 8), (16, 8)];
            Geometry::from_info(&ScreenInfo { channels, ..panel_info() }).map(|geometry| geometry.format.r_shift)
        };
        assert_eq!(
            [red(24), red(25), red(40)],
            [Ok(24), Err(Unsupported::Layout), Err(Unsupported::Layout)],
            "a colour lies within the pixel's 32 bits"
        );
        assert_eq!(
            [1919, 1916].map(|line_bytes| Geometry::from_info(&ScreenInfo { line_bytes, ..panel_info() })),
            [Err(Unsupported::Lines); 2],
            "lines of no whole pixels, and lines narrower than a page"
        );
        assert_eq!(Geometry::from_info(&ScreenInfo { height: 0, ..panel_info() }), Err(Unsupported::Lines));
    }

    #[test]
    fn ring_rotation() {
        let geometry = Geometry::from_info(&panel_info()).unwrap();
        let mut ring = Ring::new(&geometry, 640);
        assert_eq!((ring.back(), ring.back_frame()), (2, None), "the second page on screen, the third drawn into");
        ring.set_back_frame(7);
        ring.advance();
        assert_eq!((ring.back(), ring.back_frame()), (0, None), "round to the first");
        ring.set_back_frame(8);
        ring.advance();
        ring.advance();
        assert_eq!((ring.back(), ring.back_frame()), (2, Some(7)));
        ring.set_back_frame(9);
        assert_eq!(ring.back_frame(), Some(9), "a page holds the frame drawn into it whether or not the panel took it");
        ring.forget();
        assert_eq!((ring.back_frame(), Ring::new(&geometry, 1280).back()), (None, 0));
    }

    #[test]
    fn skip_back_passes_front() {
        let geometry = Geometry::from_info(&panel_info()).unwrap();
        let mut ring = Ring::new(&geometry, 1280);
        assert_eq!(ring.back(), 0, "the third page on screen, the first drawn into");
        ring.skip_back(4);
        assert_eq!((ring.back(), ring.back_frame()), (1, None), "the page drawn into shows the frame on screen");
        ring.skip_back(4);
        assert_eq!((ring.back(), ring.back_frame()), (0, Some(4)), "and the one on screen is passed by");
        let pages =
            |pages: u32| Geometry::from_info(&ScreenInfo { memory: 1920 * 640 * pages, ..panel_info() }).unwrap();
        let mut two = Ring::new(&pages(2), 640);
        two.skip_back(7);
        assert_eq!(
            (two.back(), two.back_frame()),
            (0, Some(7)),
            "of two pages the one not on screen is drawn into again"
        );
        let mut one = Ring::new(&pages(1), 0);
        one.skip_back(7);
        assert_eq!(one.back(), 0, "and of one the one there is");
        let mut turning = Ring::with_pages(3, 0);
        turning.set_back_frame(5);
        turning.advance();
        turning.skip_back(5);
        assert_eq!((turning.front(), turning.back()), (1, 0));
        turning.skip_back(5);
        assert_eq!(turning.back(), 2, "the page the panel has just turned to is passed by as the one on screen");
    }
}
