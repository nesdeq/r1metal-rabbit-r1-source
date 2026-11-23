// The sensor's colours as Rabbit's tuning measured them.

use crate::hw::camera::BLUE;
use crate::hw::camera::RED;
use crate::hw::camera::UNITY;

const TUNED_UNIT: i32 = 512;

pub(super) const fn gain(tuned: i32) -> i32 {
    tuned * UNITY / TUNED_UNIT
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct White {
    pub(super) red: i32,
    pub(super) blue: i32,
}

pub(super) const WHITES: [White; 7] = [
    White { red: 542, blue: 1231 },
    White { red: 650, blue: 1100 },
    White { red: 947, blue: 800 },
    White { red: 1028, blue: 731 },
    White { red: 1097, blue: 692 },
    White { red: 1205, blue: 633 },
    White { red: 1293, blue: 594 },
];

pub(super) const DAYLIGHT: White = WHITES[3];

struct Light {
    white: White,
    matrix: [[i32; 3]; 3],
}

const TUNING: [Light; 5] = [
    Light { white: White { red: 600, blue: 1492 }, matrix: [[675, -164, 1], [-245, 671, 86], [-221, -985, 1718]] },
    Light { white: White { red: 734, blue: 1289 }, matrix: [[759, -190, -57], [-169, 712, -31], [-54, -665, 1231]] },
    Light { white: White { red: 952, blue: 1069 }, matrix: [[799, -273, -14], [-158, 735, -65], [-20, -357, 889]] },
    Light { white: White { red: 1018, blue: 1046 }, matrix: [[968, -474, 18], [-194, 745, -39], [-17, -311, 840]] },
    Light { white: White { red: 1318, blue: 713 }, matrix: [[870, -375, 17], [-97, 763, -154], [-1, -297, 810]] },
];

fn coolness(red: i32, blue: i32) -> i64 {
    i64::from(red) * i64::from(UNITY) / i64::from(blue.max(1))
}

pub(super) fn to_blue(red: i32) -> i32 {
    let index = WHITES.iter().position(|white| white.red > red).unwrap_or(WHITES.len());
    let (low, high) = (WHITES[index.saturating_sub(1)], WHITES[index.min(WHITES.len() - 1)]);
    let span = (high.red - low.red).max(1);
    low.blue + (high.blue - low.blue) * (red - low.red).clamp(0, span) / span
}

pub(super) fn matrix(gains: [i32; 3]) -> [[i32; 3]; 3] {
    let position = coolness(gains[RED], gains[BLUE]);
    let places = TUNING.each_ref().map(|light| coolness(light.white.red, light.white.blue));
    let index = places.iter().position(|&place| place > position).unwrap_or(TUNING.len());
    let (low, high) = (index.saturating_sub(1), index.min(TUNING.len() - 1));
    let span = (places[high] - places[low]).max(1);
    let share = (position - places[low]).clamp(0, span);
    let mut mix = [[0i32; 3]; 3];
    for (row, colours) in mix.iter_mut().enumerate() {
        for (column, entry) in colours.iter_mut().enumerate() {
            let (warm, cool) =
                (i64::from(TUNING[low].matrix[row][column]), i64::from(TUNING[high].matrix[row][column]));
            *entry =
                ((warm * (span - share) + cool * share) * i64::from(UNITY) / (span * i64::from(TUNED_UNIT))) as i32;
        }
        // Whatever rounding took is given to the colour's own share, so that grey stays grey.
        let rounding = UNITY - colours.iter().sum::<i32>();
        colours[row] += rounding;
    }
    mix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance_of(white: White) -> [i32; 3] {
        [gain(white.red), UNITY, gain(white.blue)]
    }

    #[test]
    fn white_line() {
        assert!(WHITES.windows(2).all(|pair| pair[0].red < pair[1].red && pair[0].blue > pair[1].blue));
        assert_eq!(DAYLIGHT, White { red: 1028, blue: 731 });
        for white in WHITES {
            assert_eq!(to_blue(white.red), white.blue);
        }
        assert_eq!(to_blue(798), 951, "half way from incandescent to 5000 K");
        assert_eq!((to_blue(100), to_blue(5000)), (1231, 594), "past its ends the line stays at its ends");
    }

    #[test]
    fn tuned_matrices_keep_grey() {
        for light in &TUNING {
            assert!(light.matrix.iter().all(|row| row.iter().sum::<i32>() == TUNED_UNIT), "{:?}", light.matrix);
        }
        assert!(
            TUNING.windows(2).all(|pair| coolness(pair[0].white.red, pair[0].white.blue)
                < coolness(pair[1].white.red, pair[1].white.blue))
        );
    }

    #[test]
    fn matrix_mix() {
        assert_eq!(
            matrix(balance_of(TUNING[4].white)),
            [[1740, -750, 34], [-194, 1526, -308], [-2, -594, 1620]],
            "the coolest, entry by entry twice its 512ths"
        );
        assert_eq!(matrix(balance_of(TUNING[0].white)), [[1350, -328, 2], [-490, 1342, 172], [-442, -1970, 3436]]);
        assert_eq!(matrix([8000, UNITY, 1000]), matrix(balance_of(TUNING[4].white)), "cooler than the coolest");
        assert_eq!(matrix([1000, UNITY, 8000]), matrix(balance_of(TUNING[0].white)), "warmer than the warmest");
        let between = matrix(balance_of(DAYLIGHT));
        let (warm, cool) = (matrix(balance_of(TUNING[3].white)), matrix(balance_of(TUNING[4].white)));
        for row in 0..3 {
            for column in 0..3 {
                let (low, high) = (warm[row][column].min(cool[row][column]), warm[row][column].max(cool[row][column]));
                assert!(
                    (low - 1..=high + 1).contains(&between[row][column]),
                    "entry {row} {column}: {} outside {low} to {high}",
                    between[row][column]
                );
            }
        }
        assert_ne!(between, warm);
        assert_ne!(between, cool);
    }

    #[test]
    fn grey_under_any_light() {
        for red in (400..3000).step_by(37) {
            for blue in (900..3200).step_by(53) {
                let mix = matrix([red, UNITY, blue]);
                assert!(mix.iter().all(|row| row.iter().sum::<i32>() == UNITY), "gains {red} and {blue}: {mix:?}");
            }
        }
        assert_eq!(
            matrix([2 * UNITY, UNITY, UNITY]),
            matrix([4 * UNITY, 2 * UNITY, 2 * UNITY]),
            "a lift on all three gains changes nothing"
        );
    }
}
