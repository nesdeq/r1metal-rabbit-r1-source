//! The stepper that turns the camera: the angle of each way it looks and of its rest, a turn's time, and the sensor's picture turned upright for each way.

use crate::base::sampling::Turn;
use alloc::format;
use alloc::string::{String, ToString};
use core::time::Duration;

const FRONT: u32 = 0;
pub const PARKED: u32 = 90;
const REAR: u32 = 180;
const MICROS_PER_DEGREE: u64 = 6 * 410;
const SPIN_UP: Duration = Duration::from_millis(10);
const ALLOWANCE: u32 = 4;
const POLL: Duration = Duration::from_millis(5);

/// Which way the camera looks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Facing {
    // At whoever holds the device.
    Front,
    Rear,
}

/// The angle of a way of looking.
pub const fn angle(facing: Facing) -> u32 {
    match facing {
        Facing::Front => FRONT,
        Facing::Rear => REAR,
    }
}

fn timeout(start: u32, target: u32) -> Duration {
    ALLOWANCE * (SPIN_UP + Duration::from_micros(u64::from(start.abs_diff(target)) * MICROS_PER_DEGREE))
}

/// The driver file's text for a turn from `start` to `target`; none where the camera stands there.
pub fn to_order(start: u32, target: u32) -> Option<String> {
    (start != target).then(|| target.to_string())
}

/// The thread's next step about a turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    Reached,
    Wait(Duration),
    // The error text of a camera that counts as stuck.
    Late(String),
}

/// A turn on its way.
pub struct Motion {
    target: u32,
    deadline: Duration,
    is_reached: bool,
}

impl Motion {
    /// A turn from `start` to `target` that the driver was told of at `now`.
    pub fn new(start: u32, target: u32, now: Duration) -> Motion {
        Motion { target, deadline: now + timeout(start, target), is_reached: false }
    }

    /// True once the camera was found at its target.
    pub const fn is_reached(&self) -> bool {
        self.is_reached
    }

    /// Takes the driver's angle at `now`.
    pub fn next_step(&mut self, angle: u32, now: Duration) -> Step {
        if angle == self.target {
            self.is_reached = true;
            return Step::Reached;
        }
        if now > self.deadline {
            return Step::Late(format!("the camera did not reach {} degrees", self.target));
        }
        Step::Wait(POLL)
    }
}

const fn upright(facing: Facing) -> Turn {
    match facing {
        Facing::Front => Turn { quarters: 3, is_mirrored: false },
        Facing::Rear => Turn { quarters: 1, is_mirrored: false },
    }
}

/// The viewfinder's turn: upright, the front as a mirror shows it.
pub const fn viewfinder_turn(facing: Facing) -> Turn {
    Turn { is_mirrored: matches!(facing, Facing::Front), ..upright(facing) }
}

// The turn of a sensor's picture on a device held `pose_quarters` quarter turns round; front and rear see a tilt the opposite way.
pub(crate) const fn capture_turn(facing: Facing, pose_quarters: u8) -> Turn {
    let upright = upright(facing);
    let quarters = match facing {
        Facing::Front => upright.quarters + pose_quarters,
        Facing::Rear => upright.quarters + 4 - pose_quarters,
    };
    Turn { quarters: quarters % 4, ..upright }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_and_turns() {
        assert_eq!((angle(Facing::Front), angle(Facing::Rear)), (0, 180));
        assert!(angle(Facing::Front) < PARKED && PARKED < angle(Facing::Rear), "the case lies between the two");
        assert_eq!(
            upright(Facing::Front).quarters,
            (upright(Facing::Rear).quarters + 2) % 4,
            "the camera turns over between rear and front"
        );
        assert!(viewfinder_turn(Facing::Front).is_mirrored && !viewfinder_turn(Facing::Rear).is_mirrored);
        assert!(!upright(Facing::Front).is_mirrored, "a photo is no mirror image");
    }

    #[test]
    fn turn_timeout() {
        assert_eq!(
            timeout(FRONT, REAR),
            Duration::from_micros(1_811_200),
            "a half turn: 10 ms of spin up and 180 degrees of 6 steps of 410 us, four times over"
        );
        assert_eq!(timeout(PARKED, PARKED), Duration::from_millis(40), "spin up alone where the camera stands already");
        assert_eq!(timeout(FRONT, PARKED), timeout(PARKED, FRONT), "either way");
    }

    #[test]
    fn turn_ends() {
        assert_eq!((to_order(90, 90), to_order(0, 90).as_deref()), (None, Some("90")));
        let now = Duration::from_secs(1);
        let mut motion = Motion::new(PARKED, PARKED, now);
        let deadline = now + timeout(PARKED, PARKED);
        assert_eq!(motion.next_step(0, deadline), Step::Wait(POLL));
        assert_eq!(
            motion.next_step(0, deadline + POLL),
            Step::Late("the camera did not reach 90 degrees".into()),
            "looked at again after that it counts as stuck"
        );
        assert!(!motion.is_reached());
        assert_eq!(motion.next_step(PARKED, deadline + POLL), Step::Reached);
        assert!(motion.is_reached());
    }

    #[test]
    fn capture_turns() {
        let poses = [0, 1, 2, 3];
        assert_eq!(
            poses.map(|pose| capture_turn(Facing::Rear, pose).quarters),
            [1, 0, 3, 2],
            "with the button's edge up the sensor's picture stands as it is"
        );
        assert_eq!(
            poses.map(|pose| capture_turn(Facing::Front, pose).quarters),
            [3, 0, 1, 2],
            "the front camera sees the tilt the other way round"
        );
        assert!(poses.iter().all(
            |&pose| !capture_turn(Facing::Front, pose).is_mirrored && !capture_turn(Facing::Rear, pose).is_mirrored
        ));
        assert_eq!(
            (capture_turn(Facing::Rear, 0), capture_turn(Facing::Front, 0)),
            (upright(Facing::Rear), upright(Facing::Front))
        );
    }
}
