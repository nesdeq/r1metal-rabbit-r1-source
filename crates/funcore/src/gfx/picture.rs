use alloc::vec::Vec;
use core::ops::Range;

/// A picture as native pixel words.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Picture {
    pub words: Vec<u32>,
    /// Words per line.
    pub width: usize,
}

impl Picture {
    pub(crate) fn height(&self) -> usize {
        self.words.len().checked_div(self.width).unwrap_or(0)
    }
}

/// The count each of `lots` lots holds of `count` things, at least one, the last maybe fewer.
pub fn share(count: usize, lots: usize) -> usize {
    count.div_ceil(lots).max(1)
}

/// Cuts a picture of `width` samples a line into `parts` bands of whole lines: each band's lines and samples.
pub fn bands<T>(samples: &mut [T], width: usize, parts: usize) -> impl Iterator<Item = (Range<usize>, &mut [T])> {
    let width = width.max(1);
    let lines = share(samples.len() / width, parts);
    samples
        .chunks_mut(lines * width)
        .enumerate()
        .map(move |(index, band)| (index * lines..index * lines + band.len() / width, band))
}

const fn to_scaled(length: usize, numerator: usize, denominator: usize) -> usize {
    let nearest = (length * numerator + denominator / 2) / denominator;
    if nearest == 0 { 1 } else { nearest }
}

/// The largest size that fits `area` without growing; none for no size.
pub fn to_fit(size: (usize, usize), area: (usize, usize)) -> (usize, usize) {
    to_size(size, area, false, false)
}

// The largest size of its shape that fits `area`, grown where the picture is smaller; none for no size.
pub(crate) fn to_fill(size: (usize, usize), area: (usize, usize)) -> (usize, usize) {
    to_size(size, area, false, true)
}

/// The smallest size that covers `area` without growing; none for no size.
pub fn to_cover(size: (usize, usize), area: (usize, usize)) -> (usize, usize) {
    to_size(size, area, true, false)
}

fn to_size(
    (width, height): (usize, usize),
    (across, down): (usize, usize),
    is_cover: bool,
    grows: bool,
) -> (usize, usize) {
    if width == 0 || height == 0 || across == 0 || down == 0 {
        return (0, 0);
    }
    let by_height = if is_cover { width * down >= height * across } else { width * down <= height * across };
    if by_height {
        let side = if grows { down } else { height.min(down) };
        (to_scaled(width, side, height), side)
    } else {
        let side = if grows { across } else { width.min(across) };
        (side, to_scaled(height, side, width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_of_whole_lines() {
        assert_eq!([share(10, 4), share(8, 4), share(0, 4), share(3, 8)], [3, 2, 1, 1]);
        let mut words = [0u32; 3 * 10];
        let parts: Vec<(Range<usize>, usize)> =
            bands(&mut words, 3, 4).map(|(lines, band)| (lines, band.len())).collect();
        assert_eq!(parts, [(0..3, 9), (3..6, 9), (6..9, 9), (9..10, 3)], "the last band holds what is left");
        assert_eq!(bands(&mut [0u32; 2 * 3], 2, 8).count(), 3, "fewer lines than parts: a line each");
        assert_eq!(bands::<u32>(&mut [], 0, 4).count(), 0, "a picture of nothing has no band");
    }

    #[test]
    fn fit_never_grows() {
        assert_eq!(to_fit((2048, 2048), (480, 640)), (480, 480));
        assert_eq!((to_fit((1280, 720), (480, 640)), to_fit((720, 1280), (480, 640))), ((480, 270), (360, 640)));
        assert_eq!(
            (to_fit((72, 52), (480, 640)), to_fit((0, 10), (480, 640)), to_fit((4000, 10), (480, 640))),
            ((72, 52), (0, 0), (480, 1))
        );
        assert_eq!((to_fit((100, 100), (0, 50)), to_fit((100, 100), (50, 0))), ((0, 0), (0, 0)), "no area, no size");
    }

    #[test]
    fn fill_grows_and_shrinks() {
        assert_eq!(
            (to_fill((240, 320), (301, 535)), to_fill((360, 640), (301, 535)), to_fill((320, 240), (464, 321))),
            ((301, 401), (301, 535), (428, 321)),
            "a smaller picture grown until a side meets the area's, its shape kept"
        );
        assert_eq!(to_fill((1280, 720), (464, 321)), to_fit((1280, 720), (464, 321)), "a larger one shrunk as a fit");
        assert_eq!((to_fill((0, 9), (464, 321)), to_fill((9, 9), (0, 321))), ((0, 0), (0, 0)), "no size, no area");
    }

    #[test]
    fn cover_never_grows() {
        const PLACE: (usize, usize) = (228, 173);
        assert_eq!(
            to_cover((2048, 2048), PLACE),
            (228, 228),
            "a square photo: as wide as the place, and past it above and below"
        );
        assert_eq!(to_cover((720, 1280), PLACE), (228, 405), "an upright video");
        assert_eq!(
            to_cover((1280, 720), PLACE),
            (308, 173),
            "a wide one: as tall as the place, and past it left and right"
        );
        assert_eq!(to_cover((100, 50), PLACE), (100, 50), "never larger than it is");
        assert_eq!((to_cover((0, 9), PLACE), to_cover((4000, 10), PLACE)), ((0, 0), (4000, 10)));
        assert_eq!(
            (to_cover((100, 100), (0, 50)), to_cover((100, 100), (50, 0)), to_cover((100, 100), (0, 0))),
            ((0, 0), (0, 0), (0, 0)),
            "no area, no size, as for fitting"
        );
    }
}
