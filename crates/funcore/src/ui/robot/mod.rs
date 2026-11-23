mod gestures;
mod look;
mod parts;

use look::Look;

use crate::base::math::{floor, round, sine_cosine, xorshift32};
use crate::base::time::{NANOS_PER_MS, NANOS_PER_SECOND};
use crate::gfx::Rect;
use crate::gfx::icon::{Icon, Point};
use crate::gfx::mask::Mask;
use crate::gfx::palette::Roles;
use crate::gfx::pen::Pen;
use crate::ui::kit::mix_colour;
use alloc::vec::Vec;
use core::cell::RefCell;
use gestures::{
    ACTION, ADMIRE, AHEM, BLINK, BOOPED, CALM, CHEER, DROWSY, FALL_ASLEEP, FILMING, FIRST_LIGHT, FLASH, GLANCE_HOLD_MS,
    GREET, Gesture, HARKING, HUSH, IDLE, LIFTED, MUSING, NOD, PAINTING, PEEK, PEEK_ASIDE, PERK, PLUGGED, POKED, PROUD,
    RAPT, READY, RIPPLE, ROUSED, SATED, SCAN, SEEKING, SHAKE, SLEEPING, SLUMP, SPEAKING, STROKE, THINKING, UNHUSH,
    UNPLUGGED, USUAL, WAKE, WARY, WINK, WORRIED,
};
use parts::{Parts, Z_RISE, Z_SIZE};

pub(super) const DRAWING: &str = include_str!("../../../assets/icons/robot.svg");
const RESTS_NS: (u64, u64) = (2 * NANOS_PER_SECOND, 5 * NANOS_PER_SECOND);
const DROWSY_NS: u64 = 45 * NANOS_PER_SECOND;
const ASLEEP_NS: u64 = 90 * NANOS_PER_SECOND;
const SLEEP_RESTS_NS: (u64, u64) = (4 * NANOS_PER_SECOND, 9 * NANOS_PER_SECOND);
const Z_SNORE: f32 = 0.45;
const SPRING_SWINGS: f32 = 1.25;
const QUARTER: f32 = 0.25;
const WAVE_ROUNDING: f32 = 0.225;
const SEED_FLOOR: u32 = 1;
const TURN_MS: u64 = 160;
const NOTICE_NS: u64 = (TURN_MS + GLANCE_HOLD_MS as u64) * NANOS_PER_MS;
const GLARE_SHUT: f32 = 0.4;
const GLARE_PUPIL: f32 = 0.3;
const DARK_GLARE: f32 = 0.15;
const HARK_LEAN: f32 = 0.6;
const HARK_ANTENNAS: f32 = -0.5;
const WORK_RESTS_NS: (u64, u64) = (1_500 * NANOS_PER_MS, 3 * NANOS_PER_SECOND);
const SPEAK_STRETCH: f32 = 0.2;
const SPEAK_RESTS_NS: (u64, u64) = (NANOS_PER_SECOND, 2 * NANOS_PER_SECOND);
const SEARCH_RESTS_NS: (u64, u64) = (300 * NANOS_PER_MS, 600 * NANOS_PER_MS);
const PAINT_SQUINT: f32 = 0.75;
const PAINT_MOUTH: f32 = 0.6;
const FILM_SQUINT: f32 = 0.55;
const GLOW_UP_MS: u64 = 500;
const GLOW_DOWN_MS: u64 = 700;
const GLOW_PERIOD_NS: u64 = 10 * NANOS_PER_SECOND;
const GLOW_NS: u64 = (GLOW_UP_MS + GLOW_DOWN_MS) * NANOS_PER_MS;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Easing {
    Even,
    Snap,
    Fall,
    Spring,
    Steady,
}

impl Easing {
    fn share(self, time: f32) -> f32 {
        let rest = 1.0 - time;
        match self {
            Easing::Even => time * time * (3.0 - 2.0 * time),
            Easing::Snap => 1.0 - rest * rest * rest,
            Easing::Fall => time * time,
            Easing::Spring => 1.0 - rest * rest * rest * wave(SPRING_SWINGS * time + QUARTER),
            Easing::Steady => time,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mood {
    Usual,
    Worried,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Activity {
    Idle,
    Listening,
    Speaking,
    Thinking,
    Searching,
    Painting,
    Filming,
}

impl Activity {
    const fn beats(self) -> &'static [(&'static Gesture, u32)] {
        match self {
            Activity::Idle => &[],
            Activity::Listening => HARKING,
            Activity::Speaking => SPEAKING,
            Activity::Thinking => MUSING,
            Activity::Searching => SEEKING,
            Activity::Painting => PAINTING,
            Activity::Filming => FILMING,
        }
    }

    const fn opening(self) -> Option<&'static Gesture> {
        match self {
            Activity::Idle => None,
            Activity::Listening => Some(&PERK),
            Activity::Speaking => Some(&AHEM),
            Activity::Thinking => Some(&RIPPLE),
            Activity::Searching => Some(&SCAN),
            Activity::Painting => Some(&READY),
            Activity::Filming => Some(&ACTION),
        }
    }

    const fn rests(self) -> (u64, u64) {
        match self {
            Activity::Idle => RESTS_NS,
            Activity::Listening | Activity::Thinking | Activity::Painting | Activity::Filming => WORK_RESTS_NS,
            Activity::Speaking => SPEAK_RESTS_NS,
            Activity::Searching => SEARCH_RESTS_NS,
        }
    }

    fn apply(self, look: Look) -> Look {
        let [left, right] = look.eyes;
        match self {
            Activity::Idle => look,
            Activity::Listening => look.lean(HARK_LEAN).antennas(HARK_ANTENNAS, HARK_ANTENNAS),
            Activity::Speaking => look.stretch(SPEAK_STRETCH),
            Activity::Thinking => {
                let (x, y) = THINKING.gaze;
                look.gaze(x, y).pupils(THINKING.pupils).lean(THINKING.lean).mouth(THINKING.mouth)
            }
            Activity::Searching => {
                let open = look.eyes(left * WARY.eyes[0], right * WARY.eyes[1]);
                open.pupils(look.pupils * WARY.pupils).gaze(0.0, 0.0).stretch(SPEAK_STRETCH)
            }
            Activity::Painting => look.eyes(left * PAINT_SQUINT, right * PAINT_SQUINT).mouth(PAINT_MOUTH),
            Activity::Filming => look.eyes(left * FILM_SQUINT, right),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum News {
    Plugged,
    Unplugged,
    Full,
    Lifted,
    Stroke,
}

impl News {
    const fn gesture(self) -> &'static Gesture {
        match self {
            News::Plugged => &PLUGGED,
            News::Unplugged => &UNPLUGGED,
            News::Full => &SATED,
            News::Lifted => &LIFTED,
            News::Stroke => &STROKE,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Reaction {
    Flash,
    Poke,
    Boop,
    Nod,
    Shake,
    Slump,
    Proud,
    Wink,
    Admire,
    Cheer,
    Hush,
    Unhush,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Sight {
    Middle,
    Dock,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Scene {
    Free,
    Calm,
    Rapt,
}

/// Where the robot is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Seat {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) quarters: u8,
    pub(super) turn: f32,
}

impl Seat {
    // A point's offset from the seat's middle with the seat's turn taken back, as the robot drawn there has it.
    pub(super) fn offset(self, (x, y): (f32, f32)) -> (f32, f32) {
        let (offset_x, offset_y) = (x - (self.x + self.width / 2.0), y - (self.y + self.height / 2.0));
        let (sine, cosine) = sine_cosine(-f64::from(self.turn));
        let (sine, cosine) = (sine as f32, cosine as f32);
        (offset_x * cosine - offset_y * sine, offset_x * sine + offset_y * cosine)
    }

    pub(super) fn contains(self, (x, y): (i32, i32)) -> bool {
        let (offset_x, offset_y) = self.offset((x as f32, y as f32));
        let (x, y) = (self.x + self.width / 2.0 + offset_x, self.y + self.height / 2.0 + offset_y);
        Rect::new(round(self.x), round(self.y), round(self.width), round(self.height)).contains(round(x), round(y))
    }
}

fn fraction(value: f32) -> f32 {
    value - floor(value)
}

fn wave(turns: f32) -> f32 {
    let half = 2.0 * (fraction(turns + 0.5) - 0.5);
    let rough = 4.0 * half * (1.0 - half.abs());
    rough + WAVE_ROUNDING * (rough * rough.abs() - rough)
}

fn to_direction(x: f32, y: f32) -> Option<(f32, f32)> {
    let reach = x.abs().max(y.abs());
    (reach > 0.0).then(|| (x / reach, y / reach))
}

/// The kind of an outline of the robot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Piece {
    Tip,
    Body,
}

#[derive(Clone, Copy)]
enum State {
    Resting { end_ns: u64 },
    Turning { start: Look, target: Look, start_ns: u64, rest_end_ns: u64 },
    Acting { gesture: &'static Gesture, step: usize, is_mirrored: bool, start_ns: u64, start: Look },
}

/// The robot, moving in short gestures with rests between them, since every new picture keeps the display awake.
pub struct Robot {
    parts: Parts,
    look: Look,
    state: Option<State>,
    is_moving: bool,
    random: u32,
    stir_ns: Option<u64>,
    is_asleep: bool,
    pub(super) mood: Mood,
    pub(super) scene: Scene,
    pub(super) news: Option<News>,
    gaze: (f32, f32),
    pub(super) glare: f32,
    activity: Activity,
    is_opening: bool,
    pub(super) is_hushed: bool,
    notice: Option<((f32, f32), u64)>,
    z_rise: f32,
    is_beckoning: bool,
    glow_ns: Option<u64>,
    quiet_ns: u64,
    glow: f32,
    points: RefCell<Vec<Point>>,
}

impl Default for Robot {
    fn default() -> Robot {
        Robot::new()
    }
}

impl Robot {
    pub fn new() -> Robot {
        let parts = Parts::new();
        // Room for the longest outline, so that tracing a frame takes no memory.
        let points = RefCell::new(Vec::with_capacity(parts.points()));
        Robot {
            parts,
            look: Look::REST,
            state: None,
            is_moving: false,
            random: SEED_FLOOR,
            stir_ns: None,
            is_asleep: false,
            mood: Mood::Usual,
            news: None,
            scene: Scene::Free,
            gaze: (0.0, 0.0),
            glare: 0.0,
            activity: Activity::Idle,
            is_opening: false,
            is_hushed: false,
            notice: None,
            z_rise: Z_RISE,
            is_beckoning: false,
            glow_ns: None,
            quiet_ns: 0,
            glow: 0.0,
            points,
        }
    }

    fn is_glowing(&self, now_ns: u64) -> bool {
        self.glow_ns.is_some_and(|start_ns| (start_ns..start_ns + GLOW_NS).contains(&now_ns))
    }

    pub(super) fn beckon(&mut self, is_beckoning: bool, has_arrived: bool, quiet_ns: u64, now_ns: u64) {
        self.is_beckoning = is_beckoning;
        self.quiet_ns = quiet_ns;
        if is_beckoning && has_arrived && !self.is_glowing(now_ns) {
            self.glow_ns = Some(now_ns);
        }
    }

    fn glow_due(&self) -> Option<u64> {
        let last = self.glow_ns.unwrap_or(0).max(self.quiet_ns);
        self.is_beckoning.then_some(last + GLOW_PERIOD_NS)
    }

    fn glow_at(&self, now_ns: u64) -> f32 {
        let Some(start_ns) = self.glow_ns.filter(|start_ns| now_ns >= *start_ns) else {
            return 0.0;
        };
        let (ms, up, down) = ((now_ns - start_ns) as f32 / NANOS_PER_MS as f32, GLOW_UP_MS as f32, GLOW_DOWN_MS as f32);
        if ms < up {
            Easing::Even.share(ms / up)
        } else if ms < up + down {
            1.0 - Easing::Even.share((ms - up) / down)
        } else {
            0.0
        }
    }

    fn base(&self) -> Look {
        let glare = self.glare.clamp(-1.0, 1.0);
        let (shut, pupils) = if glare > 0.0 {
            (GLARE_SHUT * glare, 1.0 - GLARE_PUPIL * glare)
        } else {
            (DARK_GLARE * glare, 1.0 - DARK_GLARE * glare)
        };
        let (x, y) = self.notice.map_or(self.gaze, |notice| notice.0);
        let look = self.activity.apply(Look::REST.gaze(x, y).eyes(1.0 - shut, 1.0 - shut).pupils(pupils));
        if self.is_hushed { look.to_hush() } else { look }
    }

    pub(super) fn set_activity(&mut self, activity: Activity) {
        if core::mem::replace(&mut self.activity, activity) != activity {
            self.is_opening = activity.opening().is_some();
        }
    }

    pub(super) fn watch(&mut self, x: f32, y: f32) {
        self.gaze = to_direction(x, y).unwrap_or((0.0, 0.0));
    }

    pub(super) fn notice(&mut self, x: f32, y: f32, now_ns: u64) {
        let is_free = matches!(self.state, Some(State::Resting { .. } | State::Turning { .. })) && !self.is_asleep;
        if is_free && let Some(direction) = to_direction(x, y) {
            self.notice = Some((direction, now_ns + NOTICE_NS));
        }
    }

    pub(super) fn set_headroom(&mut self, headroom: f32) {
        let (low, high) = self.parts.extent;
        self.z_rise = headroom.max(0.0) * (high.y - low.y) + (self.parts.crown() - low.y) + Z_SIZE;
    }

    pub(super) const fn extent(&self) -> (Point, Point) {
        self.parts.extent
    }

    pub(super) fn shape(&self) -> (f32, f32) {
        let (low, high) = self.parts.extent;
        (high.x - low.x, high.y - low.y)
    }

    /// The seat at which the drawing's box is `side` pixels wide with its top left corner at (left, top).
    pub fn seat_in(&self, left: f32, top: f32, side: f32) -> Seat {
        let (low, (width, height)) = (self.parts.extent.0, self.shape());
        Seat {
            x: left + low.x * side,
            y: top + low.y * side,
            width: width * side,
            height: height * side,
            quarters: 0,
            turn: 0.0,
        }
    }

    /// True while the robot wants a new picture at every refresh.
    pub const fn is_moving(&self) -> bool {
        self.is_moving
    }

    pub const fn is_asleep(&self) -> bool {
        self.is_asleep
    }

    /// True between two gestures.
    pub const fn is_resting(&self) -> bool {
        matches!(self.state, Some(State::Resting { .. }))
    }

    /// The number of gestures it makes unprompted.
    pub const fn gestures() -> usize {
        IDLE.len()
    }

    /// Begins an unprompted gesture now, for showing each of them.
    pub fn rehearse(&mut self, number: usize, is_mirrored: bool, now_ns: u64) {
        self.seed(now_ns);
        (self.look, self.is_asleep, self.stir_ns) = (Look::REST, false, Some(now_ns));
        self.act(IDLE[number % IDLE.len()].0, is_mirrored, now_ns);
        self.is_moving = true;
    }

    const fn has_pending(&self) -> bool {
        self.news.is_some() || self.is_opening
    }

    /// The next time `advance` must run.
    pub fn deadline(&self) -> Option<u64> {
        let is_pending = self.has_pending();
        let own = match self.state? {
            State::Resting { .. } if is_pending => Some(0),
            State::Acting { gesture, .. } if is_pending && gesture.is_interruptible => Some(0),
            State::Resting { end_ns } if self.is_asleep => Some(end_ns),
            State::Resting { end_ns } => {
                let due = self.stir_ns.map_or(end_ns, |stir_ns| end_ns.min(stir_ns + ASLEEP_NS));
                Some(self.notice.map_or(due, |notice| due.min(notice.1)))
            }
            State::Turning { .. } => None,
            State::Acting { gesture, step, start_ns, .. } => {
                let wait = gesture.steps().nth(step).map(|step| u64::from(step.wait_ms) * NANOS_PER_MS);
                wait.map(|wait| start_ns + wait).filter(|_| !self.is_moving)
            }
        };
        let glow = self.glow_due().filter(|_| !self.is_moving);
        own.into_iter().chain(glow).min()
    }

    fn next_random(&mut self) -> u32 {
        self.random = xorshift32(self.random);
        self.random
    }

    fn pick_rest_ns(&mut self) -> u64 {
        let (min_ns, max_ns) = if self.is_asleep { SLEEP_RESTS_NS } else { self.activity.rests() };
        let span_ms = (max_ns - min_ns) / NANOS_PER_MS + 1;
        min_ns + u64::from(self.next_random()) % span_ms * NANOS_PER_MS
    }

    fn seed(&mut self, now_ns: u64) {
        if self.state.is_none() {
            self.random = ((now_ns ^ now_ns >> 32) as u32).max(SEED_FLOOR);
        }
    }

    fn act(&mut self, gesture: &'static Gesture, is_mirrored: bool, now_ns: u64) {
        self.look.snore = 0.0;
        self.look.ripple = 0.0;
        let is_mirrored = is_mirrored && gesture.is_mirrorable;
        self.state = Some(State::Acting { gesture, step: 0, is_mirrored, start_ns: now_ns, start: self.look });
    }

    fn idle_ns(&self, now_ns: u64) -> u64 {
        self.stir_ns.map_or(0, |stir_ns| now_ns.saturating_sub(stir_ns))
    }

    fn pick_gesture(
        &mut self,
        weights: impl Iterator<Item = (&'static Gesture, u32)> + Clone,
    ) -> (&'static Gesture, bool) {
        let total: u32 = weights.clone().map(|entry| entry.1).sum();
        let mut ticket = self.next_random() % total.max(1);
        let is_mirrored = self.next_random() & 1 == 1;
        for (gesture, weight) in weights {
            if ticket < weight {
                return (gesture, is_mirrored);
            }
            ticket -= weight;
        }
        (&BLINK, is_mirrored)
    }

    fn pick(&mut self, now_ns: u64) -> (&'static Gesture, bool) {
        let beats = self.activity.beats();
        if !beats.is_empty() {
            return self.pick_gesture(beats.iter().copied());
        }
        let column = match (self.scene, self.mood) {
            (Scene::Rapt, _) => RAPT,
            _ if self.idle_ns(now_ns) >= DROWSY_NS => DROWSY,
            (Scene::Calm, _) => CALM,
            (Scene::Free, Mood::Usual) => USUAL,
            (Scene::Free, Mood::Worried) => WORRIED,
        };
        self.pick_gesture(IDLE.iter().map(move |entry| (entry.0, entry.1[column])))
    }

    fn take_due(&mut self, end_ns: u64, now_ns: u64) -> Option<(&'static Gesture, bool)> {
        if self.has_pending() && self.is_asleep {
            self.is_asleep = false;
            return Some((&WAKE, false));
        }
        if let Some(news) = self.news.take() {
            self.stir_ns = Some(now_ns);
            return Some((news.gesture(), false));
        }
        if core::mem::take(&mut self.is_opening)
            && let Some(opening) = self.activity.opening()
        {
            self.stir_ns = Some(now_ns);
            return Some((opening, false));
        }
        if self.is_asleep {
            return (now_ns >= end_ns).then(|| self.pick_gesture(SLEEPING.iter().copied()));
        }
        if self.idle_ns(now_ns) >= ASLEEP_NS {
            self.is_asleep = true;
            return Some((&FALL_ASLEEP, false));
        }
        (now_ns >= end_ns).then(|| self.pick(now_ns))
    }

    fn run_state(&mut self, state: Option<State>, now_ns: u64) -> Option<bool> {
        match state {
            None => {
                self.seed(now_ns);
                self.stir_ns = Some(now_ns);
                self.state = Some(State::Resting { end_ns: now_ns + self.pick_rest_ns() });
            }
            Some(State::Resting { end_ns }) => {
                if let Some((gesture, is_mirrored)) = self.take_due(end_ns, now_ns) {
                    self.act(gesture, is_mirrored, now_ns);
                } else if !self.is_asleep && self.look != self.base() {
                    let target = self.base();
                    self.state =
                        Some(State::Turning { start: self.look, target, start_ns: now_ns, rest_end_ns: end_ns });
                } else {
                    return Some(false);
                }
            }
            Some(State::Turning { start, target, start_ns, rest_end_ns }) => {
                let end_ns = start_ns + TURN_MS * NANOS_PER_MS;
                let has_ended = now_ns >= end_ns;
                if target != self.base() || self.has_pending() || has_ended {
                    if has_ended {
                        self.look = target;
                    }
                    self.state = Some(State::Resting { end_ns: rest_end_ns });
                } else {
                    let share = (now_ns - start_ns) as f32 / (end_ns - start_ns) as f32;
                    self.look = start.toward(target, Easing::Even.share(share));
                    return Some(true);
                }
            }
            Some(State::Acting { gesture, .. }) if gesture.is_interruptible && self.has_pending() => {
                self.state = Some(State::Resting { end_ns: now_ns });
            }
            Some(State::Acting { gesture, step, is_mirrored, start_ns, start }) => {
                let Some(current) = gesture.steps().nth(step) else {
                    self.state = Some(State::Resting { end_ns: start_ns + self.pick_rest_ns() });
                    return None;
                };
                let target = current.aim(gesture, is_mirrored, self.base());
                let move_ns = start_ns + u64::from(current.wait_ms) * NANOS_PER_MS;
                let end_ns = move_ns + u64::from(current.duration_ms) * NANOS_PER_MS;
                if now_ns < move_ns {
                    return Some(false);
                }
                if now_ns < end_ns {
                    let share = (now_ns - move_ns) as f32 / (end_ns - move_ns) as f32;
                    self.look = start.toward(target, current.easing.share(share));
                    return Some(true);
                }
                self.look = target.to_step_end();
                let start = self.look;
                self.state = Some(State::Acting { gesture, step: step + 1, is_mirrored, start_ns: end_ns, start });
            }
        }
        None
    }

    /// Moves time on to `now_ns`.
    pub fn advance(&mut self, now_ns: u64) {
        let previous = self.look;
        self.notice = self.notice.filter(|notice| now_ns < notice.1);
        let is_active = loop {
            if let Some(is_active) = self.run_state(self.state, now_ns) {
                break is_active;
            }
        };
        if self.glow_due().is_some_and(|due| now_ns >= due) {
            self.glow_ns = Some(now_ns);
        }
        let glow = self.glow_at(now_ns);
        let has_changed = glow != core::mem::replace(&mut self.glow, glow);
        self.is_moving = is_active || self.look != previous || self.is_glowing(now_ns) || has_changed;
    }

    fn start_from(&mut self, look: Look, gesture: &'static Gesture, now_ns: u64) {
        self.look = look;
        self.news = None;
        self.perform(gesture, now_ns);
    }

    pub(super) fn greet(&mut self, now_ns: u64) {
        self.start_from(Look::REST, &GREET, now_ns);
    }

    pub(super) fn wake(&mut self, now_ns: u64) {
        self.start_from(Look::ASLEEP, &WAKE, now_ns);
    }

    pub(super) fn rouse(&mut self, now_ns: u64) {
        self.start_from(Look::ASLEEP, &ROUSED, now_ns);
    }

    pub(super) fn first_light(&mut self, now_ns: u64) {
        self.start_from(Look::ASLEEP, &FIRST_LIGHT, now_ns);
    }

    /// Notes the user's activity at `now_ns`.
    pub fn stir(&mut self, now_ns: u64) {
        if self.state.is_some() {
            self.stir_ns = Some(now_ns);
            if self.is_asleep {
                self.perform(&WAKE, now_ns);
            }
        }
    }

    pub(super) fn glance(&mut self, sight: Sight, now_ns: u64) {
        let is_free = match self.state {
            Some(State::Resting { .. } | State::Turning { .. }) => !self.is_asleep,
            Some(State::Acting { gesture, .. }) => gesture.is_interruptible,
            None => false,
        };
        if is_free {
            self.act(if sight == Sight::Middle { &PEEK } else { &PEEK_ASIDE }, false, now_ns);
            self.is_moving = true;
        }
    }

    fn perform(&mut self, gesture: &'static Gesture, now_ns: u64) {
        self.seed(now_ns);
        self.is_asleep = false;
        self.stir_ns = Some(now_ns);
        self.act(gesture, false, now_ns);
        self.is_moving = true;
    }

    pub(super) fn react(&mut self, reaction: Reaction, now_ns: u64) {
        let gesture = match reaction {
            Reaction::Flash => &FLASH,
            Reaction::Poke => POKED[self.next_random() as usize % POKED.len()],
            Reaction::Boop => &BOOPED,
            Reaction::Nod => &NOD,
            Reaction::Shake => &SHAKE,
            Reaction::Slump => &SLUMP,
            Reaction::Proud => &PROUD,
            Reaction::Wink => &WINK,
            Reaction::Admire => &ADMIRE,
            Reaction::Cheer => &CHEER,
            Reaction::Hush => &HUSH,
            Reaction::Unhush => &UNHUSH,
        };
        self.perform(gesture, now_ns);
    }

    pub(super) fn points(&self) -> usize {
        self.parts.points()
    }

    pub(super) fn asleep_icon(&self) -> Icon {
        let mut outlines = Vec::new();
        let look = Look::ASLEEP.snore(Z_SNORE);
        self.parts.outlines(&look, Z_RISE, &mut Vec::new(), |_, outline| outlines.push(outline.to_vec()));
        Icon::with_contours(outlines)
    }

    /// Adds the outlines of the pieces `is_wanted` accepts to a mask, drawn into a seat.
    pub fn trace(&self, mask: &mut Mask, seat: &Seat, is_wanted: impl Fn(Piece) -> bool) {
        let (low, (width, height)) = (self.parts.extent.0, self.shape());
        let (sine, cosine) = sine_cosine(f64::from(seat.turn));
        let (sine, bend) = (sine as f32, cosine as f32 - 1.0);
        let middle = (seat.x + seat.width / 2.0, seat.y + seat.height / 2.0);
        let to_screen = |point: &Point| {
            let (u, v) = ((point.x - low.x) / width, (point.y - low.y) / height);
            let (u, v) = match seat.quarters % 4 {
                0 => (u, v),
                1 => (1.0 - v, u),
                2 => (1.0 - u, 1.0 - v),
                _ => (v, 1.0 - u),
            };
            // No turn at all leaves every point exactly where it was.
            let (x, y) = (seat.x + u * seat.width, seat.y + v * seat.height);
            let (offset_x, offset_y) = (x - middle.0, y - middle.1);
            (x + offset_x * bend - offset_y * sine, y + offset_x * sine + offset_y * bend)
        };
        let mut points = self.points.borrow_mut();
        self.parts.outlines(&self.look, self.z_rise, &mut points, |piece, outline| {
            if is_wanted(piece) {
                mask.outline(outline.iter().map(to_screen));
            }
        });
    }

    pub(super) fn draw(&self, pen: &mut Pen<'_, '_>, roles: &Roles, seat: &Seat) {
        if self.glow <= 0.0 {
            return pen.cover(roles.robot, roles.background, |mask| self.trace(mask, seat, |_| true));
        }
        let tip = mix_colour(roles.robot, roles.text, self.glow.min(1.0));
        pen.cover(roles.robot, roles.background, |mask| self.trace(mask, seat, |piece| piece == Piece::Body));
        pen.cover_over(tip, |mask| self.trace(mask, seat, |piece| piece == Piece::Tip));
    }
}
