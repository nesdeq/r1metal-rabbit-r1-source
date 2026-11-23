//! The main loop's decisions: what it asks of the clock, the radio and the accelerometer.

pub mod cycle;
mod frames;
pub mod light;

use crate::hw::{FULL_LOAD, GOVERNED};
use crate::input::pose::Pose;
use light::Light;

/// The full clock as the loop asks for it: heavy frames get it, and so does input.
#[derive(Default)]
pub struct Boost {
    is_full: bool,
    has_input: bool,
}

impl Boost {
    const fn update(&mut self, is_awake: bool, is_heavy: bool, has_input: bool) -> Option<bool> {
        let is_wanted = is_awake && (is_heavy || has_input);
        if is_wanted == self.is_full {
            return None;
        }
        self.is_full = is_wanted;
        Some(is_wanted)
    }

    const fn take_input(&mut self, is_awake: bool, has_input: bool) -> bool {
        let is_boost = has_input && is_awake && !self.is_full;
        self.is_full |= is_boost;
        is_boost
    }

    /// The load to ask of the drawing thread's core where it changed, the full clock's or the governor's, by the frames and the input of the pass before.
    pub const fn follow(&mut self, is_awake: bool, is_heavy: bool) -> Option<u32> {
        match self.update(is_awake, is_heavy, self.has_input) {
            Some(true) => Some(FULL_LOAD),
            Some(false) => Some(GOVERNED),
            None => None,
        }
    }

    /// Takes a pass's input; true where the full clock is asked for at once, before the input is handled.
    pub const fn take_inputs<A, E>(&mut self, is_awake: bool, acts: &[A], events: &[E]) -> bool {
        self.has_input = !acts.is_empty() || !events.is_empty();
        self.take_input(is_awake, self.has_input)
    }
}

/// The token the loop watches a part under: the highest word less its place, which no input's place reaches.
pub const fn to_token(place: usize) -> u64 {
    u64::MAX - place as u64
}

/// The place of the part a ready token is of; none for an input device's.
pub fn parse_token(token: u64, parts: usize) -> Option<usize> {
    usize::try_from(u64::MAX - token).ok().filter(|place| *place < parts)
}

/// True where nothing but a sleeping system is left running.
pub fn is_quiet(parts_off: &[bool]) -> bool {
    !parts_off.contains(&false)
}

/// What the parts that go on while the device sleeps owe the radio, read once a pass: each true while it has work that needs the network.
#[derive(Clone, Copy)]
pub struct Debts {
    pub editor: bool,
    pub reader: bool,
    pub chat: bool,
    pub signal: bool,
    pub router: bool,
}

/// A part that goes on while the device sleeps only behind the radio's lifted rest, which it is granted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grantee {
    Editor,
    Signal,
}

impl Debts {
    /// True while the radio rests: the device asleep, and nothing owed.
    pub const fn is_resting(self, is_awake: bool) -> bool {
        !is_awake && !(self.editor || self.reader || self.chat || self.signal || self.router)
    }

    /// The parts the lifted rest is granted to: the editor and Signal where they owe, by the look that lifted it.
    pub fn to_grantees(self) -> impl Iterator<Item = Grantee> {
        [(self.editor, Grantee::Editor), (self.signal, Grantee::Signal)]
            .into_iter()
            .filter_map(|(is_owing, grantee)| is_owing.then_some(grantee))
    }
}

/// The pose the console named, where the screen that shows heeds the device's pose; else none, and nothing is turned or logged.
pub fn to_console_pose(does_heed: bool, pose: Option<Pose>) -> Option<Pose> {
    pose.filter(|_| does_heed)
}

/// The poses a pass takes in their order: upright where the sensor was switched, whose readings begin anew, then one the console named.
pub fn to_poses(is_switched: bool, console_pose: Option<Pose>) -> impl Iterator<Item = Pose> {
    is_switched.then_some(Pose::Upright).into_iter().chain(console_pose)
}

/// True while the accelerometer runs: for a screen that heeds the device's pose, or for the robot where it tilts with the device.
pub const fn is_tilt_wanted(heeds_tilt: bool, is_awake: bool, light: &Light, does_robot_tilt: bool) -> bool {
    heeds_tilt || (!light.is_blank(is_awake) && does_robot_tilt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_tokens() {
        assert!((0..30).all(|part| parse_token(to_token(part), 30) == Some(part)));
        assert_eq!(parse_token(0, 30), None, "the first input device's");
        assert_eq!(parse_token(to_token(30), 30), None, "past the last part");
    }

    #[test]
    fn quiet_and_radio_rest() {
        assert!(is_quiet(&[true; 13]) && !is_quiet(&[true, false, true]), "one part running");
        let none = Debts { editor: false, reader: false, chat: false, signal: false, router: false };
        assert!(none.is_resting(false) && !none.is_resting(true), "asleep with nothing owed it rests, awake never");
        assert_eq!(none.to_grantees().count(), 0);
        let owing = [
            Debts { editor: true, ..none },
            Debts { reader: true, ..none },
            Debts { chat: true, ..none },
            Debts { signal: true, ..none },
            Debts { router: true, ..none },
        ];
        assert!(owing.iter().all(|debts| !debts.is_resting(false)), "whatever is owed keeps it from its rest");
        assert_eq!(
            owing.map(|debts| debts.to_grantees().collect::<alloc::vec::Vec<Grantee>>()),
            [alloc::vec![Grantee::Editor], alloc::vec![], alloc::vec![], alloc::vec![Grantee::Signal], alloc::vec![]],
            "the editor and Signal alone go on behind the grant, each where it owes"
        );
        let both = Debts { editor: true, signal: true, ..none };
        assert!(both.to_grantees().eq([Grantee::Editor, Grantee::Signal]), "both from one look");
    }

    #[test]
    fn full_clock_requests() {
        let mut boost = Boost::default();
        assert_eq!(boost.update(true, false, false), None);
        assert!(boost.take_input(true, true) && !boost.take_input(true, true), "a bid asks for it at once, once");
        assert_eq!(boost.update(true, false, true), None, "the frame that shows it keeps it");
        assert_eq!(boost.update(true, false, false), Some(false));
        assert_eq!((boost.update(true, true, false), boost.update(false, true, true)), (Some(true), Some(false)));
        assert!(!boost.take_input(false, true), "asleep nothing hurries");
    }

    #[test]
    fn input_holds_full_clock() {
        let mut boost = Boost::default();
        assert!(boost.take_inputs(true, &[1], &[] as &[u8]));
        assert_eq!(boost.follow(true, false), None, "the frame that shows it keeps it");
        assert!(!boost.take_inputs::<u8, u8>(true, &[], &[]));
        assert_eq!(boost.follow(true, false), Some(GOVERNED), "a pass without input leaves the clock to the governor");
        assert_eq!(boost.follow(true, true), Some(FULL_LOAD), "a heavy frame asks the full clock");
    }

    #[test]
    fn pose_order() {
        let poses = |is_switched, console_pose| to_poses(is_switched, console_pose).collect::<alloc::vec::Vec<Pose>>();
        assert_eq!(poses(true, Some(Pose::LeftUp)), [Pose::Upright, Pose::LeftUp]);
        assert_eq!(poses(false, Some(Pose::LeftUp)), [Pose::LeftUp]);
        assert!(poses(false, None).is_empty());
        assert_eq!(
            [to_console_pose(true, Some(Pose::LeftUp)), to_console_pose(false, Some(Pose::LeftUp))],
            [Some(Pose::LeftUp), None],
            "a pose the console names counts on a screen that heeds the pose alone"
        );
        assert_eq!(to_console_pose(true, None), None);
    }

    #[test]
    fn tilt_wanted() {
        let mut light = Light::default();
        assert!(is_tilt_wanted(true, false, &light, false), "a screen that heeds it, asleep too");
        assert!(
            is_tilt_wanted(false, true, &light, true) && !is_tilt_wanted(false, false, &light, true),
            "for the robot that tilts, while the screen shows"
        );
        assert!(!is_tilt_wanted(false, true, &light, false), "for no robot that does not, whatever shows");
        light.set_dark(true, true);
        assert!(!is_tilt_wanted(false, true, &light, true), "not while the screen is dark under a task");
    }
}
