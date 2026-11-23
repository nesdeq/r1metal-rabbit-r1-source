use super::icon::{Icon, Point};
use super::key::Key;
use super::mask::Mask;
use super::{BLACK, Surface, WEIGHT_WHOLE, WHITE};
use crate::base::pixel::PixelFormat;
use alloc::vec;
use alloc::vec::Vec;

const WHOLE: u16 = WEIGHT_WHOLE as u16;

pub(crate) struct Stamp {
    pub(crate) side: usize,
    light: Vec<u16>,
    dark: Vec<u16>,
    pub(super) id: u64,
}

impl Stamp {
    pub(crate) fn new(icon: &Icon, size: f32, rim: usize) -> Stamp {
        let pixels = size.max(0.0) as usize;
        let side = pixels + usize::from((pixels as f32) < size) + 2 * rim;
        let cells = side * side;
        let mut words = vec![0u32; cells];
        if let Some(mut plate) = Surface::new(&mut words, side as u32, side as u32, side, PixelFormat::XRGB8888) {
            let mut mask = Mask::new(side, side);
            let inset = (side as f32 - size) / 2.0;
            let place = |point: &Point| (inset + point.x * size, inset + point.y * size);
            for contour in icon.contours() {
                mask.outline(contour.iter().map(place));
            }
            plate.cover(0, 0, &mut mask, WHITE, BLACK);
        }
        // A channel of white over black is the covered share of the pixel, of 255.
        let light: Vec<u16> = words.iter().map(|word| ((word & 0xFF) * u32::from(WHOLE) / 0xFF) as u16).collect();
        let reach = rim as i32;
        let dark: Vec<u16> = (0..cells as i32)
            .map(|at| {
                let (x, y) = (at % side as i32, at / side as i32);
                let near = (-reach..=reach).flat_map(|down| (-reach..=reach).map(move |across| (across, down)));
                near.filter(|(across, down)| across * across + down * down <= reach * reach)
                    .filter_map(|(across, down)| {
                        let (x, y) = (x + across, y + down);
                        ((0..side as i32).contains(&x) && (0..side as i32).contains(&y))
                            .then(|| light[(y * side as i32 + x) as usize])
                    })
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let weights = light.iter().chain(&dark);
        let id = weights.fold(Key::default().word(side as u64), |key, weight| key.word(u64::from(*weight))).value();
        Stamp { side, light, dark, id }
    }

    pub(super) fn row(&self, y: usize) -> (&[u16], &[u16]) {
        let line = y * self.side..(y + 1) * self.side;
        (&self.light[line.clone()], &self.dark[line])
    }

    #[cfg(test)]
    fn weights(&self, x: usize, y: usize) -> (u16, u16) {
        let (light, dark) = self.row(y);
        (light[x], dark[x])
    }
}

#[cfg(test)]
mod tests {
    use super::super::support::SQUARE;
    use super::*;

    #[test]
    fn icon_lit_with_dark_rim() {
        let stamp = Stamp::new(&Icon::from_svg(SQUARE).unwrap(), 10.0, 3);
        assert_eq!(stamp.side, 16, "the icon's box of ten pixels and the rim on either side");
        assert_eq!(
            (stamp.weights(8, 8), stamp.weights(5, 8), stamp.weights(4, 8), stamp.weights(2, 8), stamp.weights(1, 8)),
            ((WHOLE, WHOLE), (WHOLE, WHOLE), (0, WHOLE), (0, WHOLE), (0, 0)),
            "the file's square of six pixels of ten in the middle, and dark three pixels about it"
        );
        assert_eq!(
            (stamp.weights(2, 2), stamp.weights(3, 3)),
            ((0, 0), (0, WHOLE)),
            "the rim goes round the corners, as far from them as from the sides"
        );
        let lit =
            (0..16).flat_map(|y| (0..16).map(move |x| (x, y))).filter(|&(x, y)| stamp.weights(x, y).0 > 0).count();
        assert_eq!(lit, 36);
        assert_eq!(
            Stamp::new(&Icon::from_svg(SQUARE).unwrap(), 9.5, 3).side,
            16,
            "a box of part of a pixel takes a whole one"
        );
        let none = Stamp::new(&Icon::from_svg("<svg viewBox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>").unwrap(), 4.0, 2);
        assert!((0..8).all(|at| none.weights(at, at) == (0, 0)), "an icon that draws nothing darkens nothing");
        assert_eq!(Stamp::new(&Icon::from_svg(SQUARE).unwrap(), 0.0, 0).side, 0);
    }
}
