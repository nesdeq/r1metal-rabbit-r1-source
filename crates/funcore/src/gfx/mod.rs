//! Software drawing into a 32 bit framebuffer in its native channel order.

pub mod emoji;
pub mod font;
pub mod frame_stats;
pub(crate) mod glyph;
pub mod icon;
pub(crate) mod key;
pub mod mask;
pub(crate) mod measures;
pub(crate) mod motion;
pub mod palette;
pub mod panel;
pub(crate) mod pen;
mod picture;
pub mod png;
mod rect;
pub mod refresh;
mod region;
pub(crate) mod stamp;
pub(crate) mod store;
mod surface;
pub(crate) mod target;
pub mod theme;
pub mod unifont;
pub(crate) mod wrap;

pub use pen::FrameKind;
pub(crate) use picture::to_fill;
pub use picture::{Picture, bands, share, to_cover, to_fit};
pub use rect::Rect;
pub(crate) use rect::turned_size;
pub use surface::Surface;
use surface::{BLACK, WEIGHT_WHOLE, WHITE};
pub(crate) use surface::{Lettering, Stretching};

#[cfg(test)]
pub(crate) mod support {
    use crate::base::pixel::PixelFormat;

    pub(super) const SQUARE: &str = "<svg viewBox=\"0 0 10 10\"><path d=\"M2 2h6v6h-6z\"/></svg>";
    pub(crate) const RGBX: PixelFormat = PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 };

    pub(super) fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    pub(super) const fn speckle(at: u32) -> u32 {
        at.wrapping_mul(2_654_435_761) & 0x00FF_FFFF
    }
}
