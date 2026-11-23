use super::hub::PERIOD_NS;
use crate::base::time::NANOS_PER_SECOND;

const STILL_NS: u64 = 2 * NANOS_PER_SECOND;
pub(super) const STILL_READINGS: u32 = (STILL_NS / PERIOD_NS) as u32;
// Twice the largest difference between two readings of a device that lay still, 14 mm/s2 as measured over 20 s.
const STILL_RANGE: i32 = 28;
const LIFT_RANGE: i32 = 981;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Lift {
    rest: Option<((i32, i32), u32)>,
}

impl Lift {
    pub(super) fn update(&mut self, x: i32, y: i32) -> bool {
        let distance = |(at_x, at_y): (i32, i32)| (x - at_x).abs().max((y - at_y).abs());
        match self.rest {
            // Lying still, it is so until a reading is far off where it lies.
            Some((rest, count)) if count >= STILL_READINGS => {
                let is_lifted = distance(rest) > LIFT_RANGE;
                if is_lifted {
                    self.rest = Some(((x, y), 1));
                }
                is_lifted
            }
            Some((rest, count)) if distance(rest) <= STILL_RANGE => {
                self.rest = Some((rest, count + 1));
                false
            }
            _ => {
                self.rest = Some(((x, y), 1));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: (i32, i32) = (-136, 71);

    fn at_rest(count: u32) -> Lift {
        let mut lift = Lift::default();
        for step in 0..count {
            let wobble = [0, 7, -7, 14, -14][step as usize % 5];
            assert!(!lift.update(FLAT.0 + wobble, FLAT.1 - wobble), "reading {step} of a device lying still");
        }
        lift
    }

    #[test]
    fn picked_up_once() {
        let mut lift = at_rest(STILL_READINGS);
        assert!(!lift.update(FLAT.0 + LIFT_RANGE, FLAT.1), "a tenth of gravity off is not yet more than that");
        assert!(lift.update(FLAT.0, FLAT.1 + LIFT_RANGE + 1), "more than that off where it lay: picked up");
        assert!(!lift.update(FLAT.0, FLAT.1 + 3000), "told once: it is in a hand now and lies still no more");
        let mut in_hand = at_rest(STILL_READINGS);
        assert!(in_hand.update(3000, 8000), "a tilt toward whoever lifts it");
    }

    #[test]
    fn rest_time() {
        assert_eq!(u64::from(STILL_READINGS) * PERIOD_NS, STILL_NS, "two seconds of readings, however fast they come");
        let mut lift = at_rest(STILL_READINGS - 1);
        assert!(!lift.update(FLAT.0 + 5000, FLAT.1), "a reading short of two seconds of lying still is not enough");
        let mut moving = Lift::default();
        for step in 0..100 {
            let swing = if step % 2 == 0 { 40 } else { -40 };
            assert!(!moving.update(FLAT.0 + swing, FLAT.1), "a hand's tremble, {step}: never still, never picked up");
        }
        let mut settled = at_rest(STILL_READINGS);
        assert!(!settled.update(FLAT.0 + 500, FLAT.1 - 500), "a nudge where it lies is no lift");
        assert!(settled.update(FLAT.0 + 1500, FLAT.1), "and leaves it lying still where it lay");
    }

    #[test]
    fn stray_reading() {
        let mut lift = at_rest(STILL_READINGS - 5);
        assert!(!lift.update(FLAT.0 + STILL_RANGE + 1, FLAT.1));
        for _ in 0..STILL_READINGS - 2 {
            assert!(!lift.update(FLAT.0 + STILL_RANGE + 1, FLAT.1));
        }
        assert!(!lift.update(FLAT.0 + 3000, FLAT.1), "the run began anew, and is a reading short of lying still");
        let mut restart = at_rest(STILL_READINGS - 5);
        restart.update(FLAT.0 + STILL_RANGE + 1, FLAT.1);
        for _ in 0..STILL_READINGS - 1 {
            restart.update(FLAT.0 + STILL_RANGE + 1, FLAT.1);
        }
        assert!(restart.update(FLAT.0 + 3000, FLAT.1), "two seconds of readings later it lies still again");
    }
}
