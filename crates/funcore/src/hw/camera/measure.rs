//! What a frame measured while it was drawn: the light of its zones and the spread of its darkest and brightest quads.

use crate::hw::camera::GREEN;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::WHITE;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const ZONES: usize = 16;
pub(super) const DARK_CODES: usize = 64;
pub(super) const STEP_CODES: u32 = 16;
const GREEN_STEPS: usize = ((WHITE - BLACK) / STEP_CODES) as usize + 1;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Zone {
    pub(super) sums: [u32; 3],
    pub(super) quads: u32,
    pub(super) clipped: u32,
}

impl Zone {
    pub(super) fn means(&self) -> Option<[u32; 3]> {
        (self.quads > 0).then(|| self.sums.map(|sum| sum / self.quads))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Levels {
    pub(super) dark: [[u32; DARK_CODES]; 3],
    pub(super) green: [u32; GREEN_STEPS],
    pub(super) quads: u32,
}

impl Default for Levels {
    fn default() -> Levels {
        Levels { dark: [[0; DARK_CODES]; 3], green: [0; GREEN_STEPS], quads: 0 }
    }
}

impl Levels {
    #[inline]
    pub(crate) fn count(&mut self, colours: [u32; 3]) {
        for (dark, colour) in self.dark.iter_mut().zip(colours) {
            if let Some(count) = dark.get_mut(colour.saturating_sub(BLACK) as usize) {
                *count += 1;
            }
        }
        self.green[(colours[GREEN].min(WHITE).saturating_sub(BLACK) / STEP_CODES) as usize] += 1;
        self.quads += 1;
    }

    pub(super) fn dark(&self, colour: usize, one_in: u32) -> Option<u32> {
        let rank = self.quads.checked_sub(1)? / one_in;
        let mut total = 0;
        let code = self.dark[colour].iter().position(|count| {
            total += count;
            total > rank
        });
        Some(code.unwrap_or(DARK_CODES) as u32)
    }

    pub(super) fn bright(&self, one_in: u32) -> Option<u32> {
        let rank = self.quads.checked_sub(1)? / one_in;
        let mut total = 0;
        let step = self.green.iter().rposition(|count| {
            total += count;
            total > rank
        })?;
        Some(step as u32 * STEP_CODES + STEP_CODES / 2)
    }
}

/// What a frame measured: its zones by the sensor's rows, then along them, and the levels of the rows counted.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Measure {
    pub(super) zones: [[Zone; ZONES]; ZONES],
    pub(super) levels: Levels,
}

impl Measure {
    pub(super) fn sums(&self) -> [u64; 3] {
        self.zones
            .iter()
            .flatten()
            .fold([0; 3], |sums, zone| core::array::from_fn(|colour| sums[colour] + u64::from(zone.sums[colour])))
    }

    /// The quads looked at.
    pub fn quads(&self) -> u32 {
        self.zones.iter().flatten().map(|zone| zone.quads).sum()
    }

    #[cfg(test)]
    pub(super) fn clipped(&self) -> u32 {
        self.zones.iter().flatten().map(|zone| zone.clipped).sum()
    }

    pub(super) fn means(&self) -> Option<[u32; 3]> {
        let quads = self.quads();
        (quads > 0).then(|| self.sums().map(|sum| (sum / u64::from(quads)) as u32))
    }

    /// The measure of two parts of a frame taken together.
    pub fn merge(mut self, other: Measure) -> Measure {
        for (zone, part) in self.zones.iter_mut().flatten().zip(other.zones.iter().flatten()) {
            *zone = Zone {
                sums: core::array::from_fn(|colour| zone.sums[colour] + part.sums[colour]),
                quads: zone.quads + part.quads,
                clipped: zone.clipped + part.clipped,
            };
        }
        let (levels, part) = (&mut self.levels, &other.levels);
        for (dark, added) in levels.dark.iter_mut().flatten().zip(part.dark.iter().flatten()) {
            *dark += added;
        }
        for (green, added) in levels.green.iter_mut().zip(part.green) {
            *green += added;
        }
        levels.quads += part.quads;
        self
    }

    pub(crate) fn to_log_lines(self) -> impl Iterator<Item = String> {
        self.zones.into_iter().map(|row| {
            let zones = row.iter().map(|zone| {
                let [red, green, blue] = zone.means().unwrap_or_default();
                format!("{red}/{green}/{blue}{}", if zone.clipped > 0 { "*" } else { "" })
            });
            zones.collect::<Vec<_>>().join(" ")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn zone_lines() {
        let mut measure = Measure::default();
        measure.zones[0][1] = Zone { sums: [40, 80, 120], quads: 4, clipped: 1 };
        let lines: Vec<String> = measure.to_log_lines().collect();
        assert_eq!(lines.len(), ZONES);
        assert!(lines[0].starts_with("0/0/0 10/20/30* 0/0/0"), "{}", lines[0]);
        assert_eq!(lines[1], vec!["0/0/0"; ZONES].join(" "), "a zone of nothing tells nothing");
    }
}
