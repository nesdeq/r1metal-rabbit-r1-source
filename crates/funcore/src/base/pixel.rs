//! Pixels: colours and channel orders.

/// 0x00RRGGBB.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Color(pub(crate) u32);

impl Color {
    pub(crate) const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }

    pub(crate) const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub(crate) const fn b(self) -> u8 {
        self.0 as u8
    }

    pub(crate) const fn toward(self, other: Color, step: u32, steps: u32) -> Color {
        if steps == 0 {
            return self;
        }
        let step = if step < steps { step } else { steps };
        let (r, g, b) = (
            blend(self.r(), other.r(), step, steps),
            blend(self.g(), other.g(), step, steps),
            blend(self.b(), other.b(), step, steps),
        );
        Color(r << 16 | g << 8 | b)
    }
}

const fn blend(from: u8, to: u8, step: u32, steps: u32) -> u32 {
    (from as u32 * (steps - step) + to as u32 * step + steps / 2) / steps
}

/// Bit positions of the three 8 bit channels inside one native pixel word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PixelFormat {
    pub r_shift: u8,
    pub g_shift: u8,
    pub b_shift: u8,
}

impl PixelFormat {
    pub const XRGB8888: PixelFormat = PixelFormat { r_shift: 16, g_shift: 8, b_shift: 0 };

    pub(crate) const fn pack(self, color: Color) -> u32 {
        (color.r() as u32) << self.r_shift | (color.g() as u32) << self.g_shift | (color.b() as u32) << self.b_shift
    }

    #[cfg(test)]
    pub(crate) const fn unpack(self, pixel: u32) -> Color {
        let (r, g, b) = (pixel >> self.r_shift & 0xFF, pixel >> self.g_shift & 0xFF, pixel >> self.b_shift & 0xFF);
        Color(r << 16 | g << 8 | b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGBX: PixelFormat = PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 };
    const BLACK: Color = Color(0);
    const WHITE: Color = Color(0xFF_FFFF);

    #[test]
    fn formats_pack_and_unpack() {
        let color = Color(0x12_3456);
        assert_eq!((color.r(), color.g(), color.b()), (0x12, 0x34, 0x56));
        assert_eq!(PixelFormat::XRGB8888.pack(color), 0x0012_3456);
        assert_eq!(RGBX.pack(color), 0x0056_3412);
        assert_eq!(RGBX.unpack(RGBX.pack(color)), color);
    }

    #[test]
    fn colors_blend_by_steps() {
        let amber = Color(0xFF_B000);
        assert_eq!(BLACK.toward(amber, 0, 255), BLACK);
        assert_eq!(BLACK.toward(amber, 255, 255), amber);
        assert_eq!(BLACK.toward(WHITE, 128, 255), Color(0x80_8080));
        assert_eq!(WHITE.toward(BLACK, 1, 4), Color(0xBF_BFBF));
        assert_eq!(amber.toward(WHITE, 0, 0), amber, "with no steps there is no way to go");
        assert_eq!(BLACK.toward(amber, 300, 255), amber, "and past the last step none further");
    }
}
