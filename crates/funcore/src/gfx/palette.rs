//! Palettes and their colour roles.

use super::measures::{PHI, PHI2, PHI3};
use crate::base::pixel::Color;

const WHOLE: u32 = 1000;
const DIM: u32 = (WHOLE as f32 / PHI + 0.5) as u32;
const FAINT: u32 = (WHOLE as f32 / PHI2 + 0.5) as u32;
const SURFACE: u32 = (WHOLE as f32 / (PHI2 * PHI3) + 0.5) as u32;
const CONTENT_SURFACE: u32 = (WHOLE as f32 / (PHI2 * PHI2) + 0.5) as u32;
const GROUND: Color = Color(0x00_0000);
const SAVED_GREEN: Color = Color(0x55_9A00);
const ALERT_RED: Color = Color(0xFF_4300);
// A camera reads dark on light, whatever the palette.
pub(crate) const CODE_LIGHT: Color = Color(0xFF_FFFF);

/// The three base colours of a palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Palette {
    interface: Color,
    content: Color,
    mark: Color,
}

const fn palette(interface: u32, content: u32, mark: u32) -> Palette {
    Palette { interface: Color(interface), content: Color(content), mark: Color(mark) }
}

/// Palettes in menu order, A to P.
pub const PALETTES: [Palette; 16] = [
    palette(0xD4_B997, 0xF2_E8D7, 0xC0_7851),
    palette(0xAF_BFD5, 0xE4_EAF1, 0x6D_9DCE),
    palette(0xFF_E47A, 0xFF_B000, 0xF0_6A00),
    palette(0xBE_BEBE, 0xE8_E8E8, 0xFC_A942),
    palette(0xF8_B478, 0xF5_EAD9, 0xAE_7EE2),
    palette(0xFF_7A2E, 0xFF_D0B0, 0xC2_5B20),
    palette(0xFE_D8C7, 0xFE_B0A6, 0xD6_6778),
    palette(0xF5_86A7, 0xFE_DCE1, 0xC7_5783),
    palette(0xFC_D7F8, 0xEC_AFEF, 0xBF_6DCC),
    palette(0xB6_96F7, 0xEA_E1FE, 0x8E_68DC),
    palette(0x6B_B9F8, 0xD8_EDFC, 0x40_87DE),
    palette(0xC2_F2FA, 0x7A_DFF7, 0x26_A7D0),
    palette(0x52_CEC4, 0xD0_F2EF, 0x14_9D99),
    palette(0x8C_E6C4, 0xD8_EEF4, 0xAB_8BE3),
    palette(0xB8_FACE, 0x72_F08B, 0x08_BA87),
    palette(0xB4_C786, 0xEC_EBD1, 0xCE_9B2B),
];
/// The default palette: C.
pub const STANDARD: usize = 2;
const FIRST_LETTER: u8 = b'A';

const fn valid_index(index: usize) -> usize {
    if index < PALETTES.len() { index } else { STANDARD }
}

pub(crate) fn letter(index: usize) -> char {
    char::from(FIRST_LETTER + valid_index(index) as u8)
}

pub(crate) fn parse_letter(word: &str) -> Option<usize> {
    let &[byte] = word.as_bytes() else {
        return None;
    };
    let index = usize::from(byte.wrapping_sub(FIRST_LETTER));
    (index < PALETTES.len()).then_some(index)
}

const fn weaken(colour: Color, share: u32) -> Color {
    GROUND.toward(colour, share, WHOLE)
}

/// The colour roles of screens and pages.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Roles {
    pub background: Color,
    pub text: Color,
    pub(crate) text_dim: Color,
    pub(crate) rule: Color,
    pub(crate) glow: Color,
    pub robot: Color,
    pub(crate) content: Color,
    pub(crate) content_dim: Color,
    pub(crate) mark: Color,
    pub(crate) saved: Color,
    pub(crate) alert: Color,
}

impl Roles {
    /// The roles of a palette.
    pub const fn of(palette: &Palette) -> Roles {
        Roles {
            background: GROUND,
            text: palette.interface,
            text_dim: weaken(palette.interface, DIM),
            rule: weaken(palette.interface, FAINT),
            glow: palette.interface,
            robot: palette.mark,
            content: palette.content,
            content_dim: weaken(palette.content, DIM),
            mark: palette.mark,
            saved: SAVED_GREEN,
            alert: ALERT_RED,
        }
    }

    /// The roles of the palette at an index.
    pub const fn at(index: usize) -> Roles {
        Roles::of(&PALETTES[valid_index(index)])
    }

    pub(crate) const fn surface(&self) -> Color {
        weaken(self.text, SURFACE)
    }

    pub(crate) const fn content_surface(&self) -> Color {
        weaken(self.content, CONTENT_SURFACE)
    }

    pub(crate) const fn swatches(&self) -> [Color; 3] {
        [self.text, self.content, self.mark]
    }
}

impl Default for Roles {
    fn default() -> Roles {
        Roles::at(STANDARD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_letters() {
        assert_eq!((letter(0), letter(15), letter(99)), ('A', 'P', 'C'));
        assert_eq!(
            (parse_letter("A"), parse_letter("P"), parse_letter("Q"), parse_letter("AB")),
            (Some(0), Some(15), None, None)
        );
        assert_eq!(Roles::at(99), Roles::of(&PALETTES[STANDARD]));
    }
}
