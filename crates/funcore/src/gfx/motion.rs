use crate::base::time::{NANOS_PER_MS, NANOS_PER_SECOND};

const TICK_NS: u64 = NANOS_PER_MS;
const TICK_S: f32 = TICK_NS as f32 / NANOS_PER_SECOND as f32;
// Of a longer gap this much moves things and the rest is dropped instead of replayed.
const CATCH_UP_TICKS: u64 = 100;
pub(crate) const REST_DISTANCE: f32 = 0.5;
const REST_SPEED: f32 = 5.0;
pub(crate) const GRAIN: f32 = 1.0 / 256.0;
pub(crate) const STAGGER_MS: u32 = 25;

pub(crate) const SWING: Spring = spring(28.0, 0.7);
pub(crate) const PULL: Spring = spring(40.0, 1.0);
const HOLD: Spring = spring(30.0, 0.6);
const EXIT: Spring = spring(14.0, 1.0);
pub(crate) const RELEASE_SHARE: f32 = 0.9;
pub(crate) const GIVE: f32 = 0.125;
pub(crate) const EXIT_BOXES: f32 = 1.5;
// Swings about a gap far.
pub(crate) const NUDGE_SPEED: f32 = 480.0;

#[derive(Clone, Copy)]
pub(crate) struct Spring {
    stiffness: f32,
    damping: f32,
}

// `damping` 1 is critical.
pub(crate) const fn spring(rate: f32, damping: f32) -> Spring {
    Spring { stiffness: rate * rate, damping: 2.0 * damping * rate }
}

impl Spring {
    pub(crate) fn launch_speed(self, reach: f32) -> f32 {
        reach * self.damping / 2.0 * core::f32::consts::E
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct Edge {
    pub(crate) at: f32,
    pub(crate) speed: f32,
}

impl Edge {
    pub(crate) const fn still(at: f32) -> Edge {
        Edge { at, speed: 0.0 }
    }

    pub(crate) fn follow(&mut self, target: f32, spring: Spring) {
        let force = spring.stiffness * (target - self.at) - spring.damping * self.speed;
        self.speed += force * TICK_S;
        self.at += self.speed * TICK_S;
    }

    fn crosses(&mut self, target: f32, spring: Spring) -> bool {
        let before = self.at - target;
        self.follow(target, spring);
        before * (self.at - target) < 0.0
    }

    fn is_settled(&self, place: f32, grain: f32) -> bool {
        let top_speed = grain * REST_SPEED / REST_DISTANCE;
        (self.at - place).abs() < grain && self.speed.abs() < top_speed
    }

    pub(crate) fn is_at_rest(&self, place: f32) -> bool {
        self.is_settled(place, REST_DISTANCE)
    }

    pub(crate) fn settle(&mut self, target: f32, spring: Spring, grain: f32) -> bool {
        self.follow(target, spring);
        let is_moving = !self.is_settled(target, grain);
        if !is_moving {
            *self = Edge::still(target);
        }
        is_moving
    }
}

pub(crate) fn settle_row(row: &mut Edge, wait: &mut u32, target: f32, spring: Spring) -> bool {
    if *wait > 0 {
        *wait -= 1;
        return true;
    }
    row.settle(target, spring, REST_DISTANCE)
}

#[derive(Clone, Copy)]
pub(crate) struct Swing {
    pub(crate) near: Edge,
    pub(crate) far: Edge,
    pub(crate) way: f32,
    pub(crate) has_landed: bool,
    pub(crate) has_swung: bool,
}

impl Swing {
    pub(crate) const fn at_rest(near: f32, far: f32, way: f32) -> Swing {
        Swing { near: Edge::still(near), far: Edge::still(far), way, has_landed: true, has_swung: false }
    }

    pub(crate) fn front_and_rear(&mut self) -> (&mut Edge, &mut Edge) {
        if self.way < 0.0 { (&mut self.near, &mut self.far) } else { (&mut self.far, &mut self.near) }
    }

    pub(crate) fn front(&self) -> Edge {
        if self.way < 0.0 { self.near } else { self.far }
    }

    pub(crate) fn rear(&self) -> Edge {
        if self.way < 0.0 { self.far } else { self.near }
    }

    pub(crate) fn step_out(&mut self, exit: f32, length: f32, trail: Spring) {
        let (front, rear) = self.front_and_rear();
        front.follow(exit, EXIT);
        rear.follow(front.at - length, trail);
    }

    pub(crate) fn step_edges(
        &mut self,
        (front_goal, rear_goal): (f32, f32),
        spring: Spring,
        (length, trail): (f32, Spring),
    ) -> bool {
        let has_landed = self.has_landed;
        let (front, rear) = self.front_and_rear();
        let is_past = front.crosses(front_goal, spring);
        if has_landed {
            rear.follow(rear_goal, HOLD);
        } else {
            rear.follow(front.at - length, trail);
        }
        is_past
    }

    pub(crate) fn step_rest(
        &mut self,
        (front_goal, rear_goal): (f32, f32),
        is_past: bool,
        is_side_still: bool,
    ) -> bool {
        let (front, rear) = self.front_and_rear();
        let is_still = is_side_still && front.is_at_rest(front_goal) && rear.is_at_rest(rear_goal);
        if is_still {
            *front = Edge::still(front_goal);
            *rear = Edge::still(rear_goal);
        }
        self.has_landed |= is_still || (is_past && self.has_swung);
        self.has_swung |= is_past;
        is_still
    }
}

#[derive(Default)]
pub(crate) struct Pace {
    done_ns: u64,
}

impl Pace {
    pub(crate) const fn begin(&mut self, now_ns: u64) {
        self.done_ns = now_ns;
    }

    pub(crate) fn take_ticks(&mut self, now_ns: u64, is_resting: bool) -> u64 {
        let start_ns = if is_resting { now_ns } else { self.done_ns };
        let ticks = now_ns.saturating_sub(start_ns) / TICK_NS;
        self.done_ns = if ticks > CATCH_UP_TICKS { now_ns } else { start_ns + ticks * TICK_NS };
        ticks.min(CATCH_UP_TICKS)
    }
}

pub(crate) struct Clock {
    pace: Pace,
    is_resting: bool,
    is_stirred: bool,
}

impl Clock {
    pub(crate) const fn new() -> Clock {
        Clock { pace: Pace { done_ns: 0 }, is_resting: true, is_stirred: false }
    }

    pub(crate) const fn stir(&mut self) {
        self.is_stirred |= self.is_resting;
        self.is_resting = false;
    }

    pub(crate) const fn halt(&mut self) {
        self.is_resting = true;
        self.is_stirred = false;
    }

    pub(crate) const fn is_resting(&self) -> bool {
        self.is_resting
    }

    // Makes the steps due up to now, each telling whether anything still moves, and rests once one tells nothing does.
    pub(crate) fn run(&mut self, now_ns: u64, mut step: impl FnMut() -> bool) {
        for _ in 0..self.take_ticks(now_ns) {
            if self.is_resting {
                break;
            }
            self.is_resting = !step();
        }
    }

    fn take_ticks(&mut self, now_ns: u64) -> u64 {
        let is_fresh = core::mem::take(&mut self.is_stirred) || self.is_resting;
        self.pace.take_ticks(now_ns, is_fresh)
    }
}

impl Default for Clock {
    fn default() -> Clock {
        Clock::new()
    }
}

#[cfg(test)]
pub(super) mod support {
    use crate::base::time::NANOS_PER_MS;
    use crate::gfx::refresh::REFRESH_NS;

    pub(crate) const MS: u64 = NANOS_PER_MS;
    pub(crate) const FRAME_MS: u64 = REFRESH_NS / MS;

    pub(crate) fn frames(from_ms: u64, span_ms: u64, mut advance: impl FnMut(u64)) -> u64 {
        let mut now_ms = from_ms;
        while now_ms < from_ms + span_ms {
            now_ms += FRAME_MS;
            advance(now_ms * MS);
        }
        now_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_settles_at_target() {
        let (soft, firm) = (spring(28.0, 0.6), spring(24.0, 1.0));
        let (mut swinging, mut creeping) = (Edge::still(0.0), Edge::still(0.0));
        let (mut farthest, mut ticks) = (0.0f32, 0);
        while swinging.settle(100.0, soft, REST_DISTANCE) {
            farthest = farthest.max(swinging.at);
            ticks += 1;
            assert!(ticks < 1000);
        }
        assert_eq!(swinging, Edge::still(100.0));
        assert!((105.0..115.0).contains(&farthest) && (150..500).contains(&ticks));
        let mut highest = 0.0f32;
        for _ in 0..1000 {
            creeping.follow(100.0, firm);
            highest = highest.max(creeping.at);
        }
        assert!(highest <= 100.0 + 1e-3 && creeping.is_at_rest(100.0));
        assert!(
            Edge { at: 0.999, speed: 0.001 }.is_settled(1.0, GRAIN)
                && !Edge { at: 0.9, speed: 0.0 }.is_settled(1.0, GRAIN)
        );
    }

    #[test]
    fn launch_reaches_distance() {
        for (rate, reach) in [(12.0, 8.0), (30.0, 8.0), (16.0, 20.0)] {
            let mut edge = Edge { at: 0.0, speed: spring(rate, 1.0).launch_speed(reach) };
            let mut farthest = 0.0f32;
            while edge.settle(0.0, spring(rate, 1.0), REST_DISTANCE) {
                farthest = farthest.max(edge.at);
            }
            assert!(farthest <= reach && reach - farthest < reach * 2.0 * rate * TICK_S);
        }
    }

    #[test]
    fn row_waits_then_settles() {
        let (mut row, mut wait, mut ticks) = (Edge::still(0.0), 2, 0);
        assert!(settle_row(&mut row, &mut wait, 10.0, SWING) && settle_row(&mut row, &mut wait, 10.0, SWING));
        assert_eq!((row, wait), (Edge::still(0.0), 0), "it stands for every tick of its wait, which counts as moving");
        while settle_row(&mut row, &mut wait, 10.0, SWING) {
            ticks += 1;
            assert!(ticks < 1000);
        }
        assert!(ticks > 0 && row == Edge::still(10.0), "and then settles at its target");
    }

    #[test]
    fn swing_lands_at_second_pass() {
        let (soft, trail, goals) = (spring(28.0, 0.6), spring(50.0, 0.3), (40.0, 50.0));
        let down = Swing::at_rest(0.0, 10.0, 1.0);
        assert_eq!((down.front(), down.rear()), (down.far, down.near), "heading down, the far edge leads");
        let mut swing = Swing { has_landed: false, ..Swing::at_rest(100.0, 110.0, -1.0) };
        assert_eq!((swing.front(), swing.rear()), (swing.near, swing.far), "heading up, the near one");
        let (mut passes, mut landed_at, mut ticks) = (0, None, 0);
        loop {
            let is_past = swing.step_edges(goals, soft, (-10.0, trail));
            passes += u32::from(is_past);
            if swing.step_rest(goals, is_past, true) {
                break;
            }
            if swing.has_landed && landed_at.is_none() {
                landed_at = Some(passes);
            }
            ticks += 1;
            assert!(ticks < 5000);
        }
        assert_eq!(landed_at, Some(2), "it has landed once it has passed its goal a second time, on the way back");
        assert_eq!(
            (swing.near, swing.far, swing.has_swung),
            (Edge::still(40.0), Edge::still(50.0), true),
            "at rest both edges stand at their goals"
        );
        let mut side = Swing { has_landed: false, ..Swing::at_rest(40.0, 50.0, -1.0) };
        assert!(
            !side.step_rest(goals, false, false) && !side.has_landed,
            "one whose side still moves is not at rest, though its edges are"
        );
        assert!(side.step_rest(goals, false, true) && side.has_landed, "and lands as it comes to rest");
    }

    #[test]
    fn swing_leaves_with_rear_trailing() {
        let mut swing = Swing::at_rest(40.0, 50.0, -1.0);
        for _ in 0..3000 {
            swing.step_out(-15.0, -10.0, spring(50.0, 0.3));
        }
        assert!(
            swing.near.is_at_rest(-15.0) && swing.far.is_at_rest(-5.0),
            "the front goes to the exit and the rear stands its length behind: {:?}, {:?}",
            swing.near,
            swing.far
        );
    }

    #[test]
    fn pace_skips_long_gaps() {
        let mut pace = Pace::default();
        assert_eq!(pace.take_ticks(5_000_000, true), 0);
        assert_eq!(pace.take_ticks(7_500_000, false), 2);
        assert_eq!(pace.take_ticks(8_000_000, false), 1);
        assert_eq!(pace.take_ticks(900_000_000, false), CATCH_UP_TICKS);
        assert_eq!(pace.take_ticks(900_999_999, false), 0);
        pace.begin(1_000_000_000);
        assert_eq!(pace.take_ticks(1_003_000_000, false), 3);
    }

    #[test]
    fn stirred_clock_counts_from_next_step() {
        let mut clock = Clock::new();
        assert_eq!((clock.is_resting, clock.take_ticks(5_000_000)), (true, 0));
        clock.stir();
        assert_eq!((clock.is_resting, clock.take_ticks(9_000_000)), (false, 0));
        assert_eq!(clock.take_ticks(12_000_000), 3);
        clock.stir();
        assert_eq!(clock.take_ticks(14_000_000), 2);
        clock.is_resting = true;
        assert_eq!((clock.is_resting, clock.take_ticks(20_000_000)), (true, 0));
        clock.stir();
        clock.halt();
        assert_eq!((clock.is_resting, clock.take_ticks(30_000_000)), (true, 0));
    }
}
