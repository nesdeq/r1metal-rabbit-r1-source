// White balance, after libcamera's Raspberry Pi pipeline.

use crate::hw::camera::GREEN;
use crate::hw::camera::UNITY;
use crate::hw::camera::colour;
use crate::hw::camera::colour::WHITES;
use crate::hw::camera::colour::to_blue;
use crate::hw::camera::measure::Measure;
use crate::hw::camera::measure::ZONES;

const SHARE_UNIT: i64 = 1 << 10;
const FLOOR: u32 = 16;
const CLIPPED_ONE_IN: u32 = 16;
const LIMIT: i64 = SHARE_UNIT * SHARE_UNIT / 16;
const WARM_ONE_IN: i64 = 2;
const MIN_ZONES: u32 = (ZONES * ZONES / 16) as u32;
const COARSE_STEP: i32 = 16;
const FINE_STEP: i32 = 4;
const OFFSET_LOW: i32 = -48;
const OFFSET_HIGH: i32 = 96;
const OFFSET_STEP: usize = 16;
const OUTDOORS: u64 = 16_000;
const INDOORS: u64 = 160_000;
const WARMEST_OUTDOORS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Estimate {
    pub(super) gains: [i32; 2],
    pub(super) zones: u32,
    pub(super) grey: u32,
}

struct Candidates {
    shares: [(i64, i64, i64); ZONES * ZONES],
    count: usize,
}

impl Candidates {
    fn from_measure(measure: &Measure) -> Candidates {
        let mut candidates = Candidates { shares: [(0, 0, 0); ZONES * ZONES], count: 0 };
        for zone in measure.zones.iter().flatten() {
            let [red, green, blue] = zone.sums.map(i64::from);
            if zone.quads == 0 || zone.sums[GREEN] < FLOOR * zone.quads || zone.clipped * CLIPPED_ONE_IN > zone.quads {
                continue;
            }
            candidates.shares[candidates.count] =
                (red * SHARE_UNIT / green, blue * SHARE_UNIT / green, green / i64::from(zone.quads));
            candidates.count += 1;
        }
        candidates
    }

    fn score(&self, [red, blue]: [i32; 2]) -> (i64, u32) {
        let (mut sum, mut grey) = (0, 0);
        for &(red_share, blue_share, light) in &self.shares[..self.count] {
            let (red_offset, blue_offset) = (
                i64::from(red) * red_share / i64::from(UNITY) - SHARE_UNIT,
                i64::from(blue) * blue_share / i64::from(UNITY) - SHARE_UNIT,
            );
            let distance = red_offset * red_offset + blue_offset * blue_offset;
            let cap = if red_offset > blue_offset { LIMIT / WARM_ONE_IN } else { LIMIT };
            sum += light * distance.min(cap);
            grey += u32::from(distance < LIMIT);
        }
        (sum, grey)
    }
}

fn gains(red: i32, offset: i32) -> [i32; 2] {
    [red, to_blue(red)].map(|gain| colour::gain(gain) * (UNITY + offset) / UNITY)
}

fn warmest(light: u64) -> i32 {
    let (indoor, daylight) = (WHITES[0].red, WHITES[WARMEST_OUTDOORS].red);
    let dimness = light.clamp(OUTDOORS, INDOORS) - OUTDOORS;
    daylight - ((daylight - indoor) as u64 * dimness / (INDOORS - OUTDOORS)) as i32
}

pub(super) fn find_white(measure: &Measure, light: u64) -> Option<Estimate> {
    let candidates = Candidates::from_measure(measure);
    if (candidates.count as u32) < MIN_ZONES {
        return None;
    }
    let (first, last) = (warmest(light), WHITES[WHITES.len() - 1].red);
    let line = (first..last).step_by(COARSE_STEP as usize).chain([last]);
    let coarse = line.min_by_key(|&red| candidates.score(gains(red, 0)).0)?;
    let reds = (coarse - COARSE_STEP..=coarse + COARSE_STEP)
        .step_by(FINE_STEP as usize)
        .filter(|red| (first..=last).contains(red));
    let trials = reds.flat_map(|red| (OFFSET_LOW..=OFFSET_HIGH).step_by(OFFSET_STEP).map(move |offset| (red, offset)));
    let (red, offset) = trials.min_by_key(|&(red, offset)| (candidates.score(gains(red, offset)).0, offset.abs()))?;
    let (_, grey) = candidates.score(gains(red, offset));
    (grey >= MIN_ZONES).then_some(Estimate { gains: gains(red, offset), zones: candidates.count as u32, grey })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::colour::White;
    use crate::hw::camera::measure::Zone;

    const GREY: [u32; 3] = [100; 3];
    const PAPER: [u32; 3] = [300; 3];
    const LEAF: [u32; 3] = [45, 100, 30];
    const SKIN: [u32; 3] = [130, 100, 80];
    const WOOD: [u32; 3] = [125, 100, 70];
    const BRICK: [u32; 3] = [160, 100, 70];
    const SKY: [u32; 3] = [60, 100, 170];

    fn scene(white: White, things: &[([u32; 3], usize)]) -> Measure {
        const GREY_LIGHT: u32 = 2;
        const QUADS: u32 = 64;
        let mut measure = Measure::default();
        let mut zones = measure.zones.iter_mut().flatten();
        for &(colour, count) in things {
            for zone in zones.by_ref().take(count) {
                let light = colour.map(|share| share * GREY_LIGHT * QUADS);
                *zone = Zone {
                    sums: [light[0] * 512 / white.red as u32, light[1], light[2] * 512 / white.blue as u32],
                    quads: QUADS,
                    clipped: 0,
                };
            }
        }
        measure
    }

    fn white_of(measure: Measure, light: u64) -> Estimate {
        find_white(&measure, light).expect("a frame that tells its light")
    }

    fn finds(measure: Measure, light: u64, white: White, within: i32, scene_name: &str) -> Estimate {
        let estimate = white_of(measure, light);
        let offsets = [estimate.gains[0] - 2 * white.red, estimate.gains[1] - 2 * white.blue];
        assert!(
            offsets.iter().all(|offset| offset.abs() <= within),
            "{scene_name}: found {:?} for {white:?}",
            estimate.gains
        );
        estimate
    }

    #[test]
    fn grey_frame() {
        for white in WHITES {
            let estimate = finds(scene(white, &[(GREY, 256)]), INDOORS, white, 24, "greys");
            assert_eq!((estimate.zones, estimate.grey), (256, 256));
        }
    }

    #[test]
    fn few_greys() {
        let (daylight, lamp) = (WHITES[4], WHITES[1]);
        let things = [(LEAF, 150), (SKY, 40), (GREY, 50), (BRICK, 16)];
        let estimate = finds(scene(daylight, &things), OUTDOORS, daylight, 24, "out of doors");
        assert_eq!(estimate.zones, 256);
        assert!((50..=66).contains(&estimate.grey), "{} zones grey", estimate.grey);
        finds(scene(lamp, &[(BRICK, 120), (GREY, 60), (LEAF, 76)]), INDOORS, lamp, 24, "indoors");
    }

    #[test]
    fn lamp_off_line() {
        let tube = White { red: 901, blue: 992 };
        finds(scene(tube, &[(GREY, 200), (LEAF, 56)]), INDOORS, tube, 40, "a tube");
        let green_light = White { red: 1300, blue: 1300 };
        assert_eq!(
            find_white(&scene(green_light, &[(GREY, 256)]), INDOORS),
            None,
            "no lamp is that green: such a frame is a frame of green things"
        );
    }

    #[test]
    fn one_colour() {
        assert_eq!(find_white(&scene(WHITES[4], &[(LEAF, 256)]), OUTDOORS), None);
        assert_eq!(find_white(&scene(WHITES[4], &[(SKY, 256)]), INDOORS), None);
        assert_eq!(find_white(&scene(WHITES[4], &[(GREY, 15)]), INDOORS), None, "nor one with too few zones to judge");
        assert_eq!(find_white(&Measure::default(), INDOORS), None);
    }

    #[test]
    fn outdoor_skin() {
        let daylight = WHITES[4];
        let faces = scene(daylight, &[(SKIN, 180), (GREY, 76)]);
        let indoors = white_of(faces, INDOORS);
        assert!(
            indoors.gains[0] < 2 * WHITES[WARMEST_OUTDOORS].red,
            "indoors the many skin zones win: {:?}",
            indoors.gains
        );
        let outdoors = white_of(faces, OUTDOORS);
        assert!(
            outdoors.gains[0] >= 2 * WHITES[WARMEST_OUTDOORS].red * (UNITY + OFFSET_LOW) / UNITY,
            "out of doors no white is warmer than 5000 K: {:?}",
            outdoors.gains
        );
        assert_eq!((warmest(0), warmest(OUTDOORS), warmest(INDOORS), warmest(u64::MAX)), (947, 947, 542, 542));
        assert_eq!(warmest((OUTDOORS + INDOORS) / 2), 745, "half way between the two");
    }

    #[test]
    fn warm_and_dim_things() {
        let (daylight, lamp) = (WHITES[4], WHITES[1]);
        let floor = scene(daylight, &[(WOOD, 150), (GREY, 90), (LEAF, 16)]);
        finds(floor, INDOORS, daylight, 24, "a room with more wood than white in it");
        let faces = scene(daylight, &[(SKIN, 190), (PAPER, 50), (SKY, 16)]);
        finds(faces, INDOORS, daylight, 24, "skin nearly everywhere and a little white paper, three times as bright");
        let evening = scene(lamp, &[(GREY, 100), (SKY, 30), (WOOD, 126)]);
        finds(evening, INDOORS, lamp, 24, "under a lamp the greys still tell beside wood and a few bluish things");
        let blue_room = white_of(scene(lamp, &[(GREY, 100), (SKY, 60), (WOOD, 96)]), INDOORS);
        assert!(
            blue_room.gains[0] > 2 * WHITES[WARMEST_OUTDOORS].red,
            "the price of that: bluish things on more than half as many zones as the greys pass for grey things in daylight: {:?}",
            blue_room.gains
        );
    }

    #[test]
    fn dark_clipped_zones() {
        let mut measure = scene(WHITES[3], &[(GREY, 128)]);
        for zone in measure.zones.iter_mut().flatten().skip(128).take(64) {
            *zone = Zone { sums: [64 * 900, 64 * 15, 64 * 10], quads: 64, clipped: 0 };
        }
        for zone in measure.zones.iter_mut().flatten().skip(192) {
            *zone = Zone { sums: [64 * 959, 64 * 959, 64 * 400], quads: 64, clipped: 5 };
        }
        let estimate = finds(measure, INDOORS, WHITES[3], 24, "the greys alone");
        assert_eq!((estimate.zones, estimate.grey), (128, 128));
    }
}
