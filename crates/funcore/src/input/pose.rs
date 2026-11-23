//! Which way up the device is held, by its accelerometer: the pose, a lift, smoothed gravity.

use super::hub::{PERIOD_NS, RECORD_BYTES, parse_reading};
use super::lift::Lift;
use crate::base::math::atan2_turns;
use crate::base::math::round;
use alloc::format;
use alloc::string::String;

const FLAT_LIMIT: i64 = 4000;
const EDGE_RATIO: i64 = 3;
const STEADY_NS: u64 = 240_000_000;
const STEADY_READINGS: u32 = (STEADY_NS / PERIOD_NS) as u32;
const SMOOTHING_NS: u64 = 100_000_000;
/// Gravity as the accelerometer reports it at rest, in mm/s².
pub const G: i32 = 9807;

/// The edge of the device that points up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pose {
    // The top edge: held as the screen's text reads.
    #[default]
    Upright,
    // The right edge, where the button is: turned a quarter to the left.
    RightUp,
    UpsideDown,
    // The left edge: turned a quarter to the right.
    LeftUp,
}

// True where gravity in the screen's plane tells no edge: the device lies flat.
fn is_flat(x: i32, y: i32) -> bool {
    let (x, y) = (i64::from(x), i64::from(y));
    x * x + y * y < FLAT_LIMIT * FLAT_LIMIT
}

impl Pose {
    pub(crate) const fn quarters(self) -> u8 {
        match self {
            Pose::Upright => 0,
            Pose::RightUp => 1,
            Pose::UpsideDown => 2,
            Pose::LeftUp => 3,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Pose::Upright => "upright",
            Pose::RightUp => "right-up",
            Pose::UpsideDown => "upside-down",
            Pose::LeftUp => "left-up",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Pose> {
        [Pose::Upright, Pose::RightUp, Pose::UpsideDown, Pose::LeftUp].into_iter().find(|pose| pose.name() == name)
    }

    fn from_gravity(x: i32, y: i32) -> Option<Pose> {
        if is_flat(x, y) {
            return None;
        }
        let (x, y) = (i64::from(x), i64::from(y));
        let (major, minor, pose) = match (y * y >= x * x, x > 0, y > 0) {
            (true, _, true) => (y, x, Pose::Upright),
            (true, _, false) => (y, x, Pose::UpsideDown),
            (false, true, _) => (x, y, Pose::RightUp),
            (false, false, _) => (x, y, Pose::LeftUp),
        };
        (major * major > EDGE_RATIO * minor * minor).then_some(pose)
    }
}

/// The log line of a pose the readings changed, by the newest of them.
pub fn to_held_line(pose: Pose, reading: [i32; 3]) -> String {
    format!("accelerometer: held {pose:?} at {reading:?} mm/s2")
}

/// The log line of a device picked up, by the newest reading.
pub fn to_picked_up_line(reading: [i32; 3]) -> String {
    format!("accelerometer: picked up after lying still, at {reading:?} mm/s2")
}

/// The log line of a pose the console named.
pub fn to_named_line(pose: Pose) -> String {
    format!("console: held {pose:?}")
}

pub(crate) fn upright_turn(x: i32, y: i32) -> Option<f32> {
    (!is_flat(x, y)).then(|| atan2_turns(f64::from(x), f64::from(y)) as f32)
}

#[derive(Clone, Copy, Debug, Default)]
struct LowPass {
    at: Option<(f32, f32)>,
}

impl LowPass {
    fn update(&mut self, x: i32, y: i32) -> (i32, i32) {
        let share = PERIOD_NS as f32 / (SMOOTHING_NS + PERIOD_NS) as f32;
        let (x, y) = (x as f32, y as f32);
        let (smooth_x, smooth_y) =
            self.at.map_or((x, y), |(at_x, at_y)| (at_x + (x - at_x) * share, at_y + (y - at_y) * share));
        self.at = Some((smooth_x, smooth_y));
        (round(smooth_x), round(smooth_y))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PoseDebounce {
    pose: Pose,
    candidate: Option<(Pose, u32)>,
}

impl PoseDebounce {
    fn update(&mut self, x: i32, y: i32) -> Option<Pose> {
        let reading = Pose::from_gravity(x, y).filter(|&pose| pose != self.pose);
        self.candidate = match (reading, self.candidate) {
            (Some(pose), Some((candidate, count))) if pose == candidate => Some((pose, count + 1)),
            (Some(pose), _) => Some((pose, 1)),
            (None, _) => None,
        };
        let (pose, _) = self.candidate.filter(|&(_, count)| count >= STEADY_READINGS)?;
        (self.pose, self.candidate) = (pose, None);
        Some(pose)
    }
}

/// What the accelerometer's readings tell: pose, lift, smoothed gravity; a new sensor starts upright and not yet lying still, so a lift is told only after two seconds of still readings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Posture {
    debounce: PoseDebounce,
    lift: Lift,
    low_pass: LowPass,
    gravity: (i32, i32),
    last: [i32; 3],
    pose_change: Option<Pose>,
    is_lifted: bool,
}

impl Posture {
    /// Takes the records of a read of the hub's queue.
    pub fn take_records(&mut self, records: &[u8]) {
        for reading in records.as_chunks::<RECORD_BYTES>().0.iter().filter_map(parse_reading) {
            let [x, y, _] = reading;
            self.last = reading;
            self.gravity = self.low_pass.update(x, y);
            self.pose_change = self.debounce.update(x, y).or(self.pose_change);
            self.is_lifted |= self.lift.update(x, y);
        }
    }

    /// The pose if it changed since the last call, and whether the device was picked up.
    pub fn take_changes(&mut self) -> (Option<Pose>, bool) {
        (self.pose_change.take(), core::mem::take(&mut self.is_lifted))
    }

    /// The newest reading: x, y and z in mm/s².
    pub const fn last(&self) -> [i32; 3] {
        self.last
    }

    /// Smoothed gravity in the screen's plane: x and y in mm/s².
    pub const fn gravity(&self) -> (i32, i32) {
        self.gravity
    }
}

#[cfg(test)]
mod tests {
    use super::super::hub::{DATA_ACTION, record};
    use super::super::lift::STILL_READINGS;
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn gravity_poses() {
        assert_eq!(Pose::from_gravity(-136, 9811), Some(Pose::Upright), "as measured with the device standing");
        assert_eq!(Pose::from_gravity(G, 0), Some(Pose::RightUp));
        assert_eq!(Pose::from_gravity(0, -G), Some(Pose::UpsideDown));
        assert_eq!(Pose::from_gravity(-G, 0), Some(Pose::LeftUp));
        assert_eq!(Pose::from_gravity(300, -200), None, "flat on a table");
        assert_eq!(Pose::from_gravity(2800, 2800), None, "just under the flat limit");
        assert_eq!(Pose::from_gravity(6900, 6900), None, "halfway between two edges");
        assert_eq!(Pose::from_gravity(4903, 8493), Some(Pose::Upright), "30 degrees off upright is still upright");
        assert_eq!(Pose::from_gravity(5200, 8300), None, "32 degrees is in between");
        assert_eq!(Pose::from_gravity(8493, -4000), Some(Pose::RightUp));
        assert_eq!(Pose::from_gravity(0, 4100), Some(Pose::Upright), "tilted far back but not flat");
        assert_eq!([Pose::Upright, Pose::RightUp, Pose::UpsideDown, Pose::LeftUp].map(Pose::quarters), [0, 1, 2, 3]);
    }

    #[test]
    fn pose_names() {
        for pose in [Pose::Upright, Pose::RightUp, Pose::UpsideDown, Pose::LeftUp] {
            assert_eq!(Pose::from_name(pose.name()), Some(pose));
        }
        assert_eq!((Pose::RightUp.name(), Pose::from_name("sideways"), Pose::from_name("")), ("right-up", None, None));
    }

    #[test]
    fn upright_turns() {
        for (pose, x, y) in
            [(Pose::Upright, 0, G), (Pose::RightUp, G, 0), (Pose::UpsideDown, 0, -G), (Pose::LeftUp, -G, 0)]
        {
            let quarters = f32::from(pose.quarters());
            let turn = upright_turn(x, y).expect("held on an edge");
            assert!(
                turn == quarters / 4.0 || turn == quarters / 4.0 - 1.0,
                "{pose:?}: {turn}, the pose's quarter turns but for a whole one"
            );
        }
        let standing = upright_turn(-136, 9811).expect("as measured with the device standing");
        assert!(
            (standing + 0.0022).abs() < 0.0001,
            "a little anticlockwise, toward the left edge that is up a little: {standing}"
        );
        let (half_way, steep) = (upright_turn(6935, 6935).unwrap(), upright_turn(4903, 8493).unwrap());
        assert!((half_way - 0.125).abs() < 1e-6 && (steep - 1.0 / 12.0).abs() < 1e-4, "{half_way} {steep}");
        assert_eq!(
            (upright_turn(300, -200), upright_turn(2828, 2828), upright_turn(2829, 2829).is_some()),
            (None, None, true),
            "flat on a table it says nothing, as the pose does"
        );
    }

    #[test]
    fn low_pass_gravity() {
        let mut low_pass = LowPass::default();
        assert_eq!(low_pass.update(0, G), (0, G), "the first reading as it is");
        let shares: Vec<f32> = (0..30).map(|_| low_pass.update(G, 0).0 as f32 / G as f32).collect();
        assert!(
            (0.5..0.7).contains(&shares[4]) && shares[29] > 0.99,
            "most of a turn after a tenth of a second, all of it after six tenths: {} {}",
            shares[4],
            shares[29]
        );
        // A hand held upright that trembles ten times a second, a degree to either side.
        let (sway, mut steady) = (G as f32 * core::f32::consts::TAU / 360.0, LowPass::default());
        let mut widest = 0.0f32;
        for reading in 0..200u32 {
            let turn = f64::from(reading) * PERIOD_NS as f64 * 10.0 / 1e9;
            let tremor = (crate::base::math::sine_cosine(turn).0 as f32 * sway) as i32;
            let (x, _) = steady.update(tremor, G);
            if reading >= 100 {
                widest = widest.max((x as f32).abs());
            }
        }
        assert!(widest <= sway / 5.0 + 1.0, "{widest} of a sway of {sway}");
    }

    #[test]
    fn steady_pose() {
        assert_eq!(u64::from(STEADY_READINGS) * PERIOD_NS, STEADY_NS, "as many readings as come in 240 ms");
        let mut debounce = PoseDebounce::default();
        assert_eq!(debounce.pose, Pose::Upright);
        let wobble = |at: u32| [0, 100, -50][at as usize % 3];
        let poses: Vec<Option<Pose>> = (0..STEADY_READINGS).map(|at| debounce.update(G, wobble(at))).collect();
        assert!(
            poses[..STEADY_READINGS as usize - 1].iter().all(Option::is_none)
                && poses.last() == Some(&Some(Pose::RightUp)),
            "{poses:?}"
        );
        assert_eq!(debounce.pose, Pose::RightUp);
        assert_eq!(debounce.update(G, 0), None, "told once");
        for _ in 0..STEADY_READINGS - 1 {
            assert_eq!(debounce.update(0, G), None);
        }
        assert_eq!(debounce.update(7000, 7000), None, "a reading in between starts the count again");
        for _ in 0..STEADY_READINGS - 1 {
            assert_eq!(debounce.update(0, G), None);
        }
        assert_eq!(debounce.update(0, G), Some(Pose::Upright));
    }

    #[test]
    fn shakes_keep_pose() {
        let mut debounce = PoseDebounce::default();
        for _ in 0..STEADY_READINGS {
            debounce.update(-G, 0);
        }
        assert_eq!(debounce.pose, Pose::LeftUp);
        for (x, y) in [(100, 50), (0, 0), (-3000, 2000), (G, 0), (0, G), (0, -G), (G, 0), (-G, 0), (0, G)] {
            assert_eq!(debounce.update(x, y), None, "({x}, {y})");
        }
        assert_eq!(debounce.pose, Pose::LeftUp, "never long enough in a row for another edge");
    }

    fn records(readings: impl Iterator<Item = [i32; 3]>) -> Vec<u8> {
        readings.flat_map(|words| record(DATA_ACTION, words)).collect()
    }

    #[test]
    fn records_across_reads() {
        let mut posture = Posture::default();
        let right_up = records((0..STEADY_READINGS).map(|_| [G, 0, 50]));
        let (first, rest) = right_up.split_at(RECORD_BYTES * 3);
        posture.take_records(first);
        posture.take_records(&record(1, [0; 3]));
        posture.take_records(&rest[..rest.len() - 1]);
        assert_eq!(posture.take_changes(), (None, false), "a record cut short and one of another kind are no readings");
        posture.take_records(&right_up[right_up.len() - RECORD_BYTES..]);
        assert_eq!(posture.take_changes(), (Some(Pose::RightUp), false), "the readings of every read in a row count");
        assert_eq!((posture.take_changes(), posture.last(), posture.gravity()), ((None, false), [G, 0, 50], (G, 0)));
        posture.take_records(&records((0..STILL_READINGS).map(|_| [-136, 71, 9800]).chain([[3000, 8000, 0]])));
        assert_eq!(posture.take_changes(), (None, true), "lying still and then lifted, held upright");
        assert_eq!(Posture::default().take_changes(), (None, false));
    }
}
