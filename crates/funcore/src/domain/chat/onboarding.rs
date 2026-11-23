//! The first conversation: its plan and its end.

use super::memory::{Area, Memory};
use crate::base::math::xorshift64;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const TEMPLATE: &str = include_str!("../../../assets/chat/onboarding.txt");
const OPENERS: &str = include_str!("../../../assets/chat/openers.txt");
const GAMES: &str = include_str!("../../../assets/chat/games.txt");
const TONES: &str = include_str!("../../../assets/chat/tones.txt");
const GAMES_PER_PLAN: usize = 3;
const ENOUGH_AREAS: usize = 6;
const ENOUGH_PICKS: usize = 3;
pub const SEED_BYTES: usize = 16;
const SEED_FLOOR: u64 = 1;

const fn to_topic(area: Area) -> &'static str {
    match area {
        Area::Name => "their name",
        Area::Age => "their age",
        Area::Place => "where they live",
        Area::Languages => "the languages they speak",
        Area::Work => "what they do",
        Area::Interests => "what they are into",
        Area::Movies => "favourite movies",
        Area::Games => "favourite video games",
        Area::Music => "favourite music",
        Area::Media => "other favourite media: books, series, podcasts",
    }
}

fn asset_lines(asset: &'static str) -> Vec<&'static str> {
    asset.lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, count: usize) -> usize {
        self.0 = xorshift64(self.0);
        (self.0 % count.max(1) as u64) as usize
    }
}

/// The plan of one first conversation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    opener: &'static str,
    order: [Area; 9],
    games: Vec<&'static str>,
}

impl Plan {
    pub(super) fn draw(seed: &[u8; SEED_BYTES]) -> Plan {
        let (low, high) = seed.split_at(SEED_BYTES / 2);
        let half = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().expect("half the seed"));
        let mut rng = Rng((half(low) ^ half(high).rotate_left(32)).max(SEED_FLOOR));
        let openers = asset_lines(OPENERS);
        let opener = openers[rng.below(openers.len())];
        let mut order = Area::REST;
        for index in (1..order.len()).rev() {
            order.swap(index, rng.below(index + 1));
        }
        let mut pool = asset_lines(GAMES);
        let count = GAMES_PER_PLAN.min(pool.len());
        let games = (0..count).map(|_| pool.remove(rng.below(pool.len()))).collect();
        Plan { opener, order, games }
    }

    pub(super) fn to_instructions(&self) -> String {
        let order: Vec<&str> = self.order.map(to_topic).to_vec();
        TEMPLATE
            .trim()
            .replace("{OPENER}", self.opener)
            .replace("{ORDER}", &order.join(", "))
            .replace("{TONES}", &asset_lines(TONES).join(", "))
            .replace("{GAMES}", &self.games.join("; "))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Progress {
    pub(super) is_enough: bool,
    known_areas: Vec<Area>,
    open_areas: Vec<Area>,
    is_named: bool,
    has_tone: bool,
}

fn is_area_known(memory: &Memory, area: Area) -> bool {
    memory
        .profile_of(area)
        .is_some_and(|profile| profile.is_declined || !area.is_favourites() || profile.pick_count >= ENOUGH_PICKS)
}

fn to_labels(areas: &[Area]) -> String {
    let labels: Vec<&str> = areas.iter().map(|area| area.label()).collect();
    labels.join(", ")
}

impl Progress {
    pub(super) fn from_memory(memory: &Memory) -> Progress {
        let areas = core::iter::once(Area::Name).chain(Area::REST);
        let (known_areas, open_areas): (Vec<Area>, Vec<Area>) = areas.partition(|area| is_area_known(memory, *area));
        let (is_named, has_tone) = (memory.has_been_named(), memory.has_tone());
        let other_count = known_areas.iter().filter(|area| **area != Area::Name).count();
        let is_enough = known_areas.contains(&Area::Name) && is_named && has_tone && other_count >= ENOUGH_AREAS;
        Progress { is_enough, known_areas, open_areas, is_named, has_tone }
    }

    pub(super) fn to_answer(&self) -> String {
        if self.is_enough {
            return format!(
                "Noted. Known: {}. That is enough: wrap the first conversation up now, as you were told.",
                to_labels(&self.known_areas)
            );
        }
        let other_count = self.known_areas.iter().filter(|area| **area != Area::Name).count();
        let known =
            if self.known_areas.is_empty() { String::from("nothing yet") } else { to_labels(&self.known_areas) };
        let missing: Vec<&str> = [(self.is_named, "your name"), (self.has_tone, "how you should sound")]
            .into_iter()
            .filter(|offer| !offer.0)
            .map(|offer| offer.1)
            .collect();
        let prefix = if missing.is_empty() { "" } else { "; still to offer: " };
        format!(
            "Noted. Known: {known}. Open: {}{prefix}{}. {other_count} of the {ENOUGH_AREAS} areas besides the name that make enough.",
            to_labels(&self.open_areas),
            missing.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Memory {
        let mut memory = Memory::default();
        memory.set_profile(Area::Name, &["Anna".into()], false);
        for area in [Area::Age, Area::Place, Area::Languages, Area::Work, Area::Interests] {
            memory.set_profile(area, &["x".into()], false);
        }
        memory.set_profile(Area::Movies, &["a".into(), "b".into()], false);
        memory
    }

    #[test]
    fn seed_draws_plan() {
        let (first, second) = (Plan::draw(&[3; SEED_BYTES]), Plan::draw(&core::array::from_fn(|at| at as u8)));
        assert_eq!(Plan::draw(&[3; SEED_BYTES]), first);
        assert_ne!(first.order, second.order);
        assert_eq!(first.games.len(), GAMES_PER_PLAN);
        let instructions = first.to_instructions();
        assert!(!instructions.contains('{'));
        assert!(instructions.contains(first.opener));
    }

    #[test]
    fn enough_needs_picks_name_and_tone() {
        let mut memory = memory();
        assert!(!Progress::from_memory(&memory).is_enough);
        memory.set_robot_name(Some("R"), None, "d").unwrap();
        memory.set_tones(&[]);
        assert!(!Progress::from_memory(&memory).is_enough);
        memory.set_profile(Area::Movies, &["a".into(), "b".into(), "c".into()], false);
        assert!(Progress::from_memory(&memory).is_enough);
        memory.set_profile(Area::Name, &[], true);
        assert!(Progress::from_memory(&memory).is_enough);
    }

    #[test]
    fn answer_lists_known_and_open() {
        assert_eq!(
            Progress::from_memory(&Memory::default()).to_answer(),
            "Noted. Known: nothing yet. Open: Name, Age, Place, Languages, Work, Interests, Movies, Games, Music, Other media; still to offer: your name, how you should sound. 0 of the 6 areas besides the name that make enough."
        );
        let mut memory = memory();
        memory.set_robot_name(Some("R"), None, "d").unwrap();
        memory.set_tones(&[]);
        memory.set_profile(Area::Movies, &["a".into(), "b".into(), "c".into()], false);
        assert_eq!(
            Progress::from_memory(&memory).to_answer(),
            "Noted. Known: Name, Age, Place, Languages, Work, Interests, Movies. That is enough: wrap the first conversation up now, as you were told."
        );
    }
}
