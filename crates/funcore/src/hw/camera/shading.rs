use crate::hw::camera::FRACTION;
use crate::hw::camera::frame;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::WHITE;
use alloc::vec::Vec;

const POINTS: usize = 17;
const SENSOR: [u32; 2] = [frame::SENSOR.width(), frame::SENSOR.lines];
const SPACING: [u32; 2] = [SENSOR[0] / (POINTS as u32 - 1), SENSOR[1] / (POINTS as u32 - 1)];
const PLACE_BITS: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct SensorArea {
    pub(super) left: u32,
    pub(super) top: u32,
    pub(super) scale: u32,
}

pub(super) type Profile = [[i32; POINTS]; 3];

pub(super) struct Shading {
    gains: [[[u16; POINTS]; POINTS]; 3],
}

impl Shading {
    pub(super) fn profile(&self, axis: Axis, at: u32) -> Profile {
        let other = 1 - axis as usize;
        let (at, spacing) = (at.min(SENSOR[other] - 1), SPACING[other]);
        let (index, part) = ((at / spacing) as usize, (((at % spacing) << PLACE_BITS) / spacing) as i32);
        core::array::from_fn(|colour| {
            core::array::from_fn(|point| {
                let (low, high) = match axis {
                    Axis::Horizontal => (self.gains[colour][index][point], self.gains[colour][index + 1][point]),
                    Axis::Vertical => (self.gains[colour][point][index], self.gains[colour][point][index + 1]),
                };
                i32::from(low) + (((i32::from(high) - i32::from(low)) * part) >> PLACE_BITS)
            })
        })
    }
}

/// A lens's shading, and where a picture's frames lie on the sensor.
#[derive(Clone, Copy)]
pub struct Lens {
    pub(super) shading: &'static Shading,
    pub(super) area: SensorArea,
}

pub(super) struct Correction {
    pub(super) shading: &'static Shading,
    pub(super) axis: Axis,
    pub(super) span: Span,
    pub(super) lines: Vec<u32>,
}

pub(super) struct Span {
    stretches: Vec<(usize, usize, i32)>,
    step: i32,
    places: usize,
}

impl Span {
    pub(super) fn evenly(axis: Axis, first: u32, length: u32, places: usize, is_backward: bool) -> Span {
        let step = (i64::from(length) << PLACE_BITS) / places.max(1) as i64;
        let (start, end) = (i64::from(first) << PLACE_BITS, i64::from(first + length) << PLACE_BITS);
        if is_backward {
            Span::new(axis, end - step / 2, -step, places)
        } else {
            Span::new(axis, start + step / 2, step, places)
        }
    }

    fn new(axis: Axis, first: i64, step: i64, places: usize) -> Span {
        let axis = axis as usize;
        let spacing = i64::from(SPACING[axis]);
        let last = (i64::from(SENSOR[axis]) << PLACE_BITS) - 1;
        let mut stretches: Vec<(usize, usize, i32)> = Vec::new();
        for place in 0..places {
            let at = (first + place as i64 * step).clamp(0, last);
            let point = (at / (spacing << PLACE_BITS)) as usize;
            if stretches.last().is_none_or(|(_, last_point, _)| *last_point != point) {
                stretches.push((place, point, (at % (spacing << PLACE_BITS) / spacing) as i32));
            }
        }
        Span { stretches, step: (step / spacing) as i32, places }
    }

    fn apply(&self, points: &[i32; POINTS], offset: usize, slots: &mut [u16], write: impl Fn(&mut u16, i32)) {
        let end = (offset + slots.len()).min(self.places);
        let first = self.stretches.partition_point(|(place, _, _)| *place <= offset).saturating_sub(1);
        let stretches = self.stretches.iter().enumerate().skip(first).take_while(|(_, stretch)| stretch.0 < end);
        for (index, &(place, point, part)) in stretches {
            let stop = self.stretches.get(index + 1).map_or(self.places, |next| next.0).min(end);
            let start = place.max(offset);
            let rise = points[point + 1] - points[point];
            let mut gain = (points[point] << PLACE_BITS) + rise * (part + (start - place) as i32 * self.step);
            // A plain run with a gain that climbs evenly, which the compiler turns into vector code.
            for slot in &mut slots[start - offset..stop - offset] {
                write(slot, gain);
                gain += rise * self.step;
            }
        }
    }

    pub(super) fn fill(&self, points: &[i32; POINTS], offset: usize, gains: &mut [u16]) {
        self.apply(points, offset, gains, |slot, gain| *slot = (gain >> PLACE_BITS) as u16);
    }

    pub(super) fn lift(&self, points: &[i32; POINTS], offset: usize, codes: &mut [u16]) {
        self.apply(points, offset, codes, |code, gain| {
            let light = u32::from(*code).saturating_sub(BLACK) * (gain >> FRACTION) as u32;
            *code = (BLACK + (light >> PLACE_BITS)).min(WHITE) as u16;
        });
    }
}

// A code of `bits` more bits than the sensor's lifted by a gain in 1024ths, held to the sensor's maximum.
#[inline]
pub(super) fn lift_code(code: u32, gain: u16, bits: u32) -> u32 {
    let black = BLACK << bits;
    (black + ((code.saturating_sub(black) * u32::from(gain)) >> FRACTION)).min(WHITE << bits)
}

pub(super) static REFERENCE: Shading = Shading {
    gains: [
        [
            [3484, 2999, 2589, 2310, 2108, 1929, 1825, 1750, 1728, 1757, 1817, 1948, 2088, 2328, 2571, 2961, 3432],
            [3182, 2744, 2399, 2130, 1913, 1769, 1637, 1574, 1554, 1582, 1656, 1788, 1932, 2154, 2396, 2666, 3112],
            [2951, 2546, 2226, 1970, 1752, 1597, 1479, 1416, 1386, 1414, 1483, 1596, 1773, 1968, 2236, 2544, 2868],
            [2771, 2384, 2073, 1825, 1627, 1461, 1354, 1289, 1271, 1290, 1351, 1453, 1624, 1835, 2087, 2384, 2688],
            [2613, 2257, 1954, 1709, 1497, 1348, 1251, 1194, 1175, 1192, 1253, 1349, 1501, 1717, 1960, 2247, 2591],
            [2487, 2182, 1867, 1619, 1419, 1268, 1177, 1118, 1104, 1121, 1175, 1268, 1420, 1630, 1857, 2188, 2489],
            [2446, 2114, 1815, 1546, 1357, 1214, 1131, 1075, 1056, 1073, 1124, 1215, 1356, 1553, 1807, 2108, 2442],
            [2384, 2078, 1780, 1518, 1322, 1190, 1095, 1049, 1032, 1049, 1092, 1181, 1333, 1523, 1773, 2069, 2385],
            [2384, 2067, 1763, 1507, 1313, 1170, 1086, 1038, 1025, 1036, 1087, 1171, 1308, 1513, 1764, 2051, 2375],
            [2390, 2082, 1778, 1526, 1326, 1185, 1089, 1043, 1028, 1040, 1095, 1176, 1322, 1519, 1783, 2077, 2401],
            [2430, 2119, 1798, 1562, 1355, 1210, 1115, 1069, 1055, 1069, 1119, 1214, 1347, 1553, 1792, 2106, 2409],
            [2499, 2180, 1868, 1615, 1420, 1260, 1166, 1113, 1093, 1113, 1173, 1268, 1421, 1613, 1887, 2179, 2503],
            [2599, 2275, 1960, 1700, 1508, 1347, 1245, 1177, 1166, 1184, 1245, 1342, 1501, 1691, 1960, 2281, 2591],
            [2715, 2382, 2087, 1801, 1612, 1458, 1340, 1282, 1262, 1288, 1349, 1455, 1610, 1812, 2072, 2395, 2666],
            [2909, 2542, 2242, 1969, 1766, 1585, 1476, 1406, 1386, 1416, 1471, 1587, 1747, 1955, 2226, 2535, 2867],
            [3178, 2738, 2412, 2133, 1921, 1755, 1640, 1564, 1548, 1571, 1634, 1748, 1920, 2143, 2378, 2683, 3085],
            [3513, 2947, 2593, 2336, 2113, 1945, 1833, 1764, 1736, 1751, 1828, 1954, 2103, 2327, 2571, 2916, 3700],
        ],
        [
            [3193, 2700, 2349, 2115, 1934, 1791, 1699, 1640, 1619, 1647, 1710, 1811, 1958, 2134, 2369, 2697, 3170],
            [2902, 2483, 2186, 1970, 1784, 1652, 1554, 1497, 1476, 1497, 1555, 1668, 1806, 1983, 2219, 2491, 2872],
            [2676, 2308, 2030, 1829, 1650, 1506, 1414, 1360, 1338, 1355, 1419, 1520, 1658, 1839, 2060, 2319, 2631],
            [2512, 2171, 1911, 1696, 1531, 1392, 1299, 1248, 1234, 1251, 1304, 1398, 1542, 1721, 1938, 2201, 2483],
            [2374, 2072, 1818, 1604, 1438, 1302, 1214, 1167, 1154, 1170, 1220, 1303, 1442, 1613, 1840, 2110, 2379],
            [2296, 1999, 1748, 1529, 1361, 1232, 1155, 1106, 1091, 1104, 1155, 1237, 1370, 1545, 1761, 2023, 2297],
            [2233, 1945, 1700, 1479, 1309, 1186, 1109, 1064, 1052, 1064, 1108, 1196, 1323, 1497, 1712, 1969, 2248],
            [2195, 1934, 1662, 1454, 1285, 1162, 1087, 1041, 1032, 1044, 1087, 1167, 1286, 1458, 1684, 1948, 2223],
            [2185, 1922, 1655, 1450, 1277, 1155, 1079, 1036, 1024, 1038, 1080, 1159, 1278, 1447, 1678, 1928, 2210],
            [2217, 1937, 1676, 1459, 1284, 1165, 1086, 1046, 1031, 1048, 1088, 1166, 1289, 1465, 1689, 1949, 2214],
            [2247, 1968, 1708, 1490, 1319, 1191, 1112, 1068, 1050, 1066, 1117, 1202, 1323, 1496, 1719, 1974, 2259],
            [2299, 2026, 1759, 1545, 1372, 1246, 1157, 1110, 1094, 1114, 1161, 1244, 1372, 1549, 1769, 2023, 2314],
            [2389, 2097, 1841, 1619, 1454, 1309, 1225, 1172, 1155, 1174, 1223, 1318, 1450, 1624, 1847, 2118, 2386],
            [2497, 2201, 1939, 1710, 1540, 1402, 1312, 1255, 1240, 1253, 1313, 1399, 1537, 1722, 1945, 2205, 2497],
            [2664, 2325, 2072, 1832, 1655, 1521, 1425, 1368, 1349, 1364, 1422, 1519, 1662, 1843, 2064, 2339, 2660],
            [2926, 2496, 2219, 1988, 1805, 1668, 1573, 1514, 1492, 1508, 1569, 1673, 1802, 1989, 2216, 2502, 2882],
            [3290, 2715, 2381, 2150, 1969, 1833, 1732, 1674, 1650, 1672, 1732, 1832, 1974, 2148, 2378, 2693, 3437],
        ],
        [
            [2919, 2508, 2181, 1982, 1820, 1691, 1599, 1566, 1555, 1573, 1632, 1719, 1847, 1987, 2222, 2487, 2938],
            [2654, 2299, 2051, 1844, 1689, 1560, 1483, 1441, 1427, 1430, 1499, 1590, 1705, 1865, 2056, 2321, 2679],
            [2447, 2149, 1903, 1714, 1566, 1446, 1360, 1320, 1303, 1314, 1378, 1458, 1570, 1731, 1944, 2183, 2473],
            [2309, 2025, 1812, 1613, 1466, 1357, 1264, 1228, 1209, 1227, 1278, 1358, 1481, 1649, 1841, 2085, 2332],
            [2211, 1940, 1711, 1521, 1384, 1267, 1197, 1151, 1136, 1150, 1194, 1273, 1393, 1550, 1740, 1981, 2240],
            [2130, 1870, 1660, 1467, 1313, 1208, 1136, 1096, 1082, 1098, 1139, 1208, 1335, 1498, 1681, 1913, 2152],
            [2088, 1838, 1608, 1423, 1280, 1173, 1098, 1061, 1048, 1062, 1105, 1179, 1292, 1442, 1632, 1865, 2121],
            [2057, 1803, 1589, 1392, 1242, 1143, 1077, 1038, 1031, 1041, 1085, 1158, 1268, 1413, 1615, 1856, 2105],
            [2058, 1797, 1575, 1391, 1245, 1136, 1068, 1030, 1026, 1036, 1075, 1146, 1253, 1408, 1593, 1838, 2090],
            [2057, 1834, 1591, 1402, 1259, 1142, 1081, 1035, 1030, 1041, 1084, 1159, 1263, 1418, 1617, 1850, 2114],
            [2123, 1851, 1622, 1440, 1282, 1178, 1100, 1062, 1051, 1062, 1106, 1186, 1288, 1447, 1644, 1873, 2136],
            [2169, 1902, 1672, 1472, 1329, 1212, 1142, 1098, 1089, 1096, 1150, 1217, 1336, 1492, 1692, 1936, 2202],
            [2244, 1972, 1752, 1560, 1406, 1288, 1205, 1158, 1151, 1160, 1212, 1286, 1406, 1567, 1771, 2016, 2273],
            [2330, 2067, 1837, 1646, 1484, 1367, 1288, 1240, 1225, 1239, 1289, 1370, 1489, 1661, 1872, 2123, 2362],
            [2478, 2207, 1952, 1761, 1608, 1475, 1398, 1340, 1318, 1347, 1390, 1467, 1603, 1786, 1985, 2212, 2505],
            [2707, 2341, 2090, 1904, 1742, 1606, 1517, 1471, 1459, 1467, 1519, 1609, 1730, 1901, 2108, 2365, 2721],
            [3043, 2545, 2252, 2055, 1880, 1769, 1680, 1614, 1601, 1608, 1669, 1756, 1890, 2065, 2265, 2540, 3194],
        ],
    ],
};

#[cfg(test)]
pub(super) mod fake {
    use super::*;
    use alloc::boxed::Box;

    pub(crate) fn lens(gain: impl Fn(usize, usize, usize) -> u16) -> Lens {
        let gains = core::array::from_fn(|colour| {
            core::array::from_fn(|row| core::array::from_fn(|column| gain(colour, row, column)))
        });
        Lens { shading: Box::leak(Box::new(Shading { gains })), area: SensorArea { left: 0, top: 0, scale: 1 } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::BLUE;
    use crate::hw::camera::GREEN;
    use crate::hw::camera::RED;
    use crate::hw::camera::UNITY;
    use alloc::vec;

    const UNIT: i64 = 1 << PLACE_BITS;

    #[test]
    fn reference_lens() {
        let gains = &REFERENCE.gains;
        assert!(gains.iter().flatten().flatten().all(|&gain| i32::from(gain) >= UNITY), "no gain takes light away");
        let middle = POINTS / 2;
        assert_eq!([RED, GREEN, BLUE].map(|colour| gains[colour][middle][middle]), [1025, 1024, 1026]);
        for colour in [RED, GREEN, BLUE] {
            let corners = [gains[colour][0][0], gains[colour][0][16], gains[colour][16][0], gains[colour][16][16]];
            let largest = gains[colour].iter().flatten().max().unwrap();
            assert!(corners.contains(largest) && corners.iter().all(|&corner| corner > 2900), "{corners:?}");
            for row in &gains[colour] {
                assert!(row[..=middle].windows(2).all(|pair| pair[0] + 16 > pair[1]), "falling to the middle: {row:?}");
                assert!(row[middle..].windows(2).all(|pair| pair[0] < pair[1] + 16), "rising from it: {row:?}");
            }
        }
        assert!(
            (0..POINTS).all(|at| gains[RED][0][at] > gains[GREEN][0][at] && gains[GREEN][0][at] > gains[BLUE][0][at]),
            "along the sensor's first row red is lifted most and blue least"
        );
        assert_eq!(SPACING, [204, 153]);
    }

    #[test]
    fn profile_between_grid_lines() {
        let lens = fake::lens(|colour, row, column| (1000 * (colour + 1) + 100 * row + column) as u16);
        let row = lens.shading.profile(Axis::Horizontal, 0);
        assert_eq!((row[RED][0], row[RED][16], row[BLUE][3]), (1000, 1016, 3003), "the grid's first row");
        assert_eq!(lens.shading.profile(Axis::Horizontal, 153)[GREEN][5], 2105, "its second row lies 153 pixels down");
        assert_eq!(
            lens.shading.profile(Axis::Horizontal, 153 + 51)[GREEN][5],
            2105 + 33,
            "a third of the way to the third"
        );
        assert_eq!(
            lens.shading.profile(Axis::Horizontal, 9999)[RED][2],
            2502 + 100 * 152 / 153,
            "past the sensor's end: its last row of pixels, just short of the grid's last"
        );
        let column = lens.shading.profile(Axis::Vertical, 204);
        assert_eq!((column[RED][0], column[RED][16]), (1001, 2601), "down the grid's second column");
        assert_eq!(
            lens.shading.profile(Axis::Vertical, 102)[RED][4],
            1400,
            "halfway between the first two, rounded down"
        );
    }

    #[test]
    fn gains_along_a_line() {
        let mut points = [0i32; POINTS];
        for (point, gain) in points.iter_mut().enumerate() {
            *gain = 1024 + 100 * (point * point) as i32;
        }
        let span = Span::new(Axis::Horizontal, 0, 51 * UNIT, 65);
        let mut gains = vec![0u16; 65];
        span.fill(&points, 0, &mut gains);
        assert_eq!(gains[..9], [1024, 1049, 1074, 1099, 1124, 1199, 1274, 1349, 1424], "four places to a grid step");
        assert_eq!(gains[64], 1024 + 100 * 256, "the last place lies on the last grid point");
        let mut parts = vec![0u16; 65];
        for (start, stop) in [(0, 3), (3, 4), (4, 30), (30, 65)] {
            span.fill(&points, start, &mut parts[start..stop]);
        }
        assert_eq!(parts, gains, "filled in parts, wherever they are cut");
        let back = Span::new(Axis::Horizontal, 3264 * UNIT - 1, -51 * UNIT, 65);
        let mut mirrored = vec![0u16; 65];
        back.fill(&points, 0, &mut mirrored);
        assert!(
            mirrored.iter().rev().zip(&gains).all(|(a, b)| a.abs_diff(*b) <= 4),
            "a line that runs the other way has the same gains in the other order: {:?}",
            &mirrored[60..]
        );
        let column = Span::new(Axis::Vertical, 0, 153 * UNIT, 17);
        let mut steps = vec![0u16; 17];
        column.fill(&points, 0, &mut steps);
        assert!(steps.iter().zip(&points).all(|(gain, point)| i32::from(*gain) == *point), "153 pixels a step down");
        let mut none = [7u16; 2];
        span.fill(&points, 65, &mut none[..0]);
        assert_eq!(none, [7; 2]);
        let (mut shared, mut turned) = ([0u16; 16], [0u16; 16]);
        Span::evenly(Axis::Horizontal, 0, 3264, 16, false).fill(&points, 0, &mut shared);
        assert_eq!(
            shared[..3],
            [1024 + 50, 1124 + 150, 1424 + 250],
            "places that share the sensor's width lie in the middle of their shares"
        );
        Span::evenly(Axis::Horizontal, 0, 3264, 16, true).fill(&points, 0, &mut turned);
        assert!(turned.iter().rev().zip(&shared).all(|(a, b)| a.abs_diff(*b) <= 1), "{turned:?}");
        let mut inner = [0u16; 2];
        Span::evenly(Axis::Horizontal, 204, 408, 2, false).fill(&points, 0, &mut inner);
        assert_eq!(inner, [1124 + 150, 1424 + 250], "a part of the width, from where it begins");
    }

    #[test]
    fn code_lift() {
        assert_eq!(lift_code(BLACK, 3000, 0), BLACK);
        assert_eq!(lift_code(10, 3000, 0), BLACK, "what lies under black is black");
        assert_eq!(lift_code(BLACK + 100, 2048, 0), BLACK + 200);
        assert_eq!(lift_code(BLACK + 100, 1536, 0), BLACK + 150);
        assert_eq!(lift_code(BLACK + 600, 2048, 0), WHITE);
        assert!((BLACK..=WHITE).all(|code| lift_code(code, 1024, 0) == code), "a gain of one changes nothing");
        assert_eq!(
            (lift_code((BLACK + 100) << 4 | 8, 2048, 4), lift_code(WHITE << 4, 2048, 4)),
            ((BLACK + 201) << 4, WHITE << 4),
            "a code in sixteenths, as a merge has it, keeps its sixteenths"
        );
        let points: [i32; POINTS] = core::array::from_fn(|point| 1024 + 128 * point as i32);
        let span = Span::evenly(Axis::Horizontal, 0, 3264, 64, false);
        let mut gains = [0u16; 64];
        span.fill(&points, 0, &mut gains);
        let raw: [u16; 64] = core::array::from_fn(|place| 16 * place as u16);
        let mut codes = raw;
        span.lift(&points, 0, &mut codes[..40]);
        span.lift(&points, 40, &mut codes[40..]);
        assert!(
            codes
                .iter()
                .zip(raw)
                .zip(gains)
                .all(|((code, raw), gain)| u32::from(*code) == lift_code(raw.into(), gain, 0)),
            "a run of codes lifted along a line, in parts: each by the gain of its place"
        );
        assert_eq!((codes[0], codes[5], codes[63]), (BLACK as u16, 64 + 16 + 2, WHITE as u16));
    }
}
