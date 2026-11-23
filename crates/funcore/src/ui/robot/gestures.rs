use super::Easing;
use super::look::Look;
use super::parts::RIPPLE_TOTAL_MS;

const BREATH_MS: u32 = 3000;
pub(super) const USUAL: usize = 0;
pub(super) const WORRIED: usize = 1;
pub(super) const DROWSY: usize = 2;
pub(super) const CALM: usize = 3;
pub(super) const RAPT: usize = 4;
const MOODS: usize = 5;
pub(super) const GLANCE_HOLD_MS: u32 = 450;
const PING_SPREAD: f32 = 0.5;

pub(super) struct Step {
    pub(super) wait_ms: u32,
    pub(super) duration_ms: u32,
    pub(super) easing: Easing,
    target: Look,
}

impl Step {
    pub(super) fn aim(&self, gesture: &Gesture, is_mirrored: bool, base: Look) -> Look {
        match self.target {
            target if target == Look::REST => base,
            target if gesture.is_overlay => (if is_mirrored { target.to_mirror() } else { target }).on_base(base),
            target if is_mirrored => target.to_mirror(),
            target => target,
        }
    }
}

const fn step(wait_ms: u32, duration_ms: u32, target: Look) -> Step {
    Step { wait_ms, duration_ms, easing: Easing::Even, target }
}

const fn snap(wait_ms: u32, duration_ms: u32, target: Look) -> Step {
    Step { wait_ms, duration_ms, easing: Easing::Snap, target }
}

const fn fall(wait_ms: u32, duration_ms: u32, target: Look) -> Step {
    Step { wait_ms, duration_ms, easing: Easing::Fall, target }
}

const fn spring(wait_ms: u32, duration_ms: u32, target: Look) -> Step {
    Step { wait_ms, duration_ms, easing: Easing::Spring, target }
}

const fn steady(wait_ms: u32, duration_ms: u32, target: Look) -> Step {
    Step { wait_ms, duration_ms, easing: Easing::Steady, target }
}

pub(super) struct Gesture {
    pub(super) is_mirrorable: bool,
    pub(super) is_interruptible: bool,
    is_overlay: bool,
    phrases: &'static [&'static [Step]],
}

impl Gesture {
    const fn new(phrases: &'static [&'static [Step]]) -> Gesture {
        Gesture { is_mirrorable: false, is_interruptible: false, is_overlay: false, phrases }
    }

    const fn overlay(self) -> Gesture {
        Gesture { is_overlay: true, ..self }
    }

    const fn mirrorable(self) -> Gesture {
        Gesture { is_mirrorable: true, ..self }
    }

    const fn interruptible(self) -> Gesture {
        Gesture { is_interruptible: true, ..self }
    }

    pub(super) fn steps(&self) -> impl Iterator<Item = &'static Step> + use<> {
        let phrases = self.phrases;
        phrases.iter().flat_map(|phrase| phrase.iter())
    }
}

const SHUT: Look = Look::REST.eyes(0.0, 0.0);
pub(super) const WARY: Look = Look::REST.eyes(1.1, 1.1).pupils(0.8);
const SCARED: Look = Look::REST.eyes(1.2, 1.2).pupils(0.5).mouth(1.3);
const FROZEN: Look = Look::REST.eyes(1.2, 1.2).pupils(0.55).stretch(0.4).antennas(-0.6, 0.6);
const PHEW: Look = Look::REST.eyes(0.6, 0.6).stretch(-0.4).mouth(0.8).antennas(-0.3, 0.3);
const GLAD: Look = Look::REST.smile(1.0).eyes(0.6, 0.6).mouth(1.15);
const EEK: Look = Look::REST.eyes(1.12, 1.12).pupils(0.7).mouth(1.3);
const ASIDE: Look = Look::REST.gaze(1.0, 0.0).pupils(0.85).head(0.5, 0.0).lean(0.3).antennas(0.4, 0.4);
const UP: Look = Look::REST.gaze(0.0, -1.0).pupils(0.85);
pub(super) const THINKING: Look = Look::REST.gaze(-0.5, -0.85).pupils(0.85).lean(-0.5).mouth(0.55);
const AT_IT: Look = Look::REST.gaze(0.75, -0.65).pupils(0.8);
const DOWN: Look = Look::REST.gaze(0.0, 1.0).pupils(0.85).head(0.0, 0.5);
const PUZZLED: Look = Look::REST.lean(0.9).eyes(1.15, 0.8).gaze(0.5, -0.3).pupils(0.85).mouth(0.7).antennas(0.6, 0.5);
const BEAMING: Look = GLAD.eyes(0.35, 0.35);
const CONTENT: Look = Look::REST.smile(0.6).eyes(0.45, 0.45).mouth(0.5);

const GLIMPSE: &[Step] = &[step(0, 170, DOWN)];
const RELIEF: &[Step] = &[step(600, 220, Look::REST.eyes(0.6, 0.6).smile(0.7)), spring(450, 260, Look::REST)];
const fn tremble(eek: Look) -> [Step; 2] {
    [step(0, 60, eek.mouth(0.7).head(0.6, 0.0)), step(0, 60, eek.head(-0.6, 0.0))]
}
const GULPING: Look = Look::REST.pupils(0.65).mouth(0.15).stretch(-0.5).antennas(-0.3, 0.3);
const SWALLOW: &[Step] = &[step(350, 150, GULPING), spring(480, 300, Look::REST)];
const REJOICE: &[Step] = &[
    step(250, 150, GLAD.stretch(-0.5).antennas(-0.5, 0.5)),
    snap(0, 100, GLAD.stretch(0.4).head(0.0, -0.7).antennas(0.6, -0.6)),
    fall(0, 100, GLAD.stretch(-0.5).antennas(-0.6, 0.6)),
    snap(0, 100, GLAD.stretch(0.3).head(0.0, -0.5).antennas(0.4, -0.4)),
    fall(0, 100, GLAD.stretch(-0.3)),
    spring(350, 280, Look::REST),
];
const CONTENTED: &[Step] = &[
    step(400, 260, CONTENT.eyes(0.3, 0.3).stretch(0.3).head(0.0, -0.2)),
    step(300, 380, CONTENT.stretch(-0.35).antennas(-0.3, 0.3)),
    spring(250, 280, Look::REST),
];
const DROOP: &[Step] = &[
    step(0, 420, Look::REST.mouth(1.4).eyes(0.15, 0.15).stretch(0.6).head(0.0, -0.2).antennas(-0.5, 0.5)),
    step(200, 450, Look::REST.eyes(0.35, 0.35).head(0.0, 0.3).antennas(-0.3, 0.3)),
    snap(300, 90, Look::REST.eyes(0.9, 0.9)),
    step(250, 600, Look::REST.eyes(0.0, 0.0).mouth(0.6).head(0.0, 0.5).stretch(-0.2).antennas(-0.6, 0.6)),
    step(0, 700, Look::ASLEEP),
];
const BREATHE: &[Step] = &[
    steady(300, BREATH_MS / 2, Look::ASLEEP.snore(0.5).stretch(-0.25).mouth(0.3)),
    steady(0, BREATH_MS / 2, Look::ASLEEP.snore(1.0)),
];
const START: &[Step] = &[
    snap(0, 70, SCARED.stretch(0.9).head(0.0, -0.3).antennas(-0.9, 0.9)),
    step(60, 130, FROZEN),
    snap(380, 80, FROZEN.gaze(-1.0, 0.0)),
    snap(280, 110, FROZEN.gaze(1.0, 0.0)),
    step(300, 240, PHEW),
    spring(320, 280, Look::REST),
];
const BLINK_AWAKE: &[Step] = &[
    step(0, 260, Look::REST.eyes(0.35, 0.35).head(0.0, 0.3).stretch(-0.2)),
    fall(160, 70, SHUT),
    snap(40, 120, WARY.stretch(0.2)),
];
const BLINKING: &[Step] = &[fall(0, 70, SHUT), snap(40, 110, Look::REST)];
const PINGING: &[Step] = &[snap(0, 70, Look::REST.antennas(-PING_SPREAD, PING_SPREAD)), spring(0, 280, Look::REST)];
const FOLLOWING: &[Step] = &[step(0, 120, Look::REST.head(0.0, 1.0).stretch(-0.2)), spring(0, 260, Look::REST)];
const HUMMING: &[Step] = &[
    step(0, 160, Look::REST.mouth(0.5).head(0.0, 0.8)),
    step(0, 160, Look::REST.mouth(0.8).head(0.0, -0.3)),
    spring(0, 240, Look::REST),
];

pub(super) const BLINK: Gesture = Gesture::new(&[BLINKING]);
const BLINK_TWICE: Gesture =
    Gesture::new(&[&[fall(0, 70, SHUT), snap(40, 90, Look::REST), fall(110, 70, SHUT), snap(40, 110, Look::REST)]]);
const DART: Gesture = Gesture::new(&[&[
    snap(0, 90, WARY.gaze(1.0, 0.0).head(0.3, 0.0).antennas(0.3, 0.3)),
    snap(380, 120, WARY.gaze(-1.0, 0.0).head(-0.3, 0.0).antennas(-0.3, -0.3)),
    step(320, 170, Look::REST),
]])
.mirrorable();
const LOOK_ASIDE: Gesture = Gesture::new(&[&[
    step(0, 190, ASIDE),
    fall(520, 70, ASIDE.eyes(0.0, 0.0)),
    snap(40, 100, ASIDE),
    spring(450, 260, Look::REST),
]])
.mirrorable();
const PONDER: Gesture = Gesture::new(&[&[
    step(0, 220, THINKING),
    step(600, 140, THINKING.antennas(0.5, 0.5)),
    step(0, 150, THINKING.antennas(-0.3, -0.3)),
    spring(300, 280, Look::REST),
]])
.mirrorable();
const WIGGLE: Gesture = Gesture::new(&[&[
    step(0, 130, UP.antennas(0.9, 0.9)),
    step(0, 160, UP.antennas(-0.9, -0.9)),
    step(0, 160, UP.antennas(0.7, 0.7)),
    step(0, 150, UP.antennas(-0.5, -0.5)),
    spring(0, 260, Look::REST),
]]);
const TWITCH: Gesture = Gesture::new(&[&[
    snap(0, 70, Look::REST.antennas(0.0, 0.9)),
    step(0, 90, Look::REST.antennas(0.0, -0.2)),
    snap(140, 70, AT_IT.antennas(0.0, 0.8)),
    step(0, 110, AT_IT),
    step(380, 130, AT_IT.eyes(0.55, 0.55)),
    spring(320, 220, Look::REST),
]])
.mirrorable();
pub(super) const NOD: Gesture = Gesture::new(&[&[
    step(0, 110, Look::REST.head(0.0, 1.0).stretch(-0.3).antennas(-0.5, 0.5)),
    step(0, 110, Look::REST.head(0.0, -0.4).antennas(0.3, -0.3)),
    step(0, 110, Look::REST.head(0.0, 0.7).stretch(-0.2).antennas(-0.4, 0.4)),
    spring(0, 240, Look::REST),
]]);
pub(super) const SHAKE: Gesture = Gesture::new(&[&[
    step(0, 90, Look::REST.eyes(0.25, 0.25).head(-1.0, 0.0).antennas(0.7, 0.7)),
    step(0, 130, Look::REST.eyes(0.25, 0.25).head(1.0, 0.0).antennas(-0.7, -0.7)),
    step(0, 130, Look::REST.eyes(0.25, 0.25).head(-0.8, 0.0).antennas(0.6, 0.6)),
    step(0, 120, Look::REST.eyes(0.25, 0.25).head(0.6, 0.0).antennas(-0.4, -0.4)),
    spring(0, 240, Look::REST),
]])
.mirrorable();
const CHATTER: Gesture =
    Gesture::new(&[&[snap(0, 80, EEK)], &tremble(EEK), &tremble(EEK), &tremble(EEK), &[spring(0, 260, Look::REST)]]);
const GULP: Gesture = Gesture::new(&[&[step(0, 150, GULPING), spring(480, 300, Look::REST)]]);
const STARTLE: Gesture = Gesture::new(&[START]);
const HOP: Gesture = Gesture::new(&[&[
    step(0, 110, Look::REST.stretch(-1.0).antennas(-0.6, 0.6)),
    snap(0, 100, Look::REST.stretch(0.6).head(0.0, -0.9).antennas(0.5, -0.5).eyes(1.1, 1.1)),
    fall(40, 100, Look::REST.stretch(-0.8).antennas(-0.7, 0.7)),
    spring(0, 280, Look::REST),
]]);
const BOUNCE: Gesture = Gesture::new(&[&[
    step(0, 90, Look::REST.stretch(-0.7).antennas(-0.5, 0.5)),
    snap(0, 90, Look::REST.stretch(0.4).head(0.0, -0.7).antennas(0.4, -0.4)),
    fall(0, 90, Look::REST.stretch(-0.6).antennas(-0.6, 0.6)),
    snap(0, 90, Look::REST.stretch(0.3).head(0.0, -0.5).antennas(0.3, -0.3)),
    fall(0, 90, Look::REST.stretch(-0.45).antennas(-0.4, 0.4)),
    spring(0, 240, Look::REST),
]]);
const WOBBLE: Gesture = Gesture::new(&[&[
    step(0, 130, Look::REST.lean(0.7).antennas(-0.5, -0.5)),
    step(0, 170, Look::REST.lean(-0.7).antennas(0.5, 0.5)),
    step(0, 160, Look::REST.lean(0.45).antennas(-0.4, -0.4)),
    step(0, 150, Look::REST.lean(-0.25).antennas(0.2, 0.2)),
    spring(0, 220, Look::REST),
]])
.mirrorable();
const SHIMMY: Gesture = Gesture::new(&[&[
    step(0, 70, Look::REST.head(-1.0, 0.0).antennas(0.6, 0.6)),
    step(0, 90, Look::REST.head(1.0, 0.0).antennas(-0.6, -0.6)),
    step(0, 90, Look::REST.head(-1.0, 0.0).antennas(0.6, 0.6)),
    step(0, 90, Look::REST.head(1.0, 0.0).antennas(-0.6, -0.6)),
    step(0, 90, Look::REST.head(-0.5, 0.0).antennas(0.3, 0.3)),
    spring(0, 200, Look::REST),
]])
.mirrorable();
const SWAY: Gesture = Gesture::new(&[&[
    step(0, 240, Look::REST.head(-0.8, 0.0).lean(-0.4).gaze(-0.7, 0.0).antennas(0.3, 0.3)),
    step(0, 320, Look::REST.head(0.8, 0.0).lean(0.4).gaze(0.7, 0.0).antennas(-0.3, -0.3)),
    spring(0, 280, Look::REST),
]])
.mirrorable();
pub(super) const CHEER: Gesture = Gesture::new(&[REJOICE]);
const DANCING: &[Step] = &[
    step(0, 110, GLAD.lean(0.5).stretch(-0.5).antennas(-0.6, 0.2)),
    step(0, 110, GLAD.lean(-0.5).stretch(0.3).head(0.0, -0.5).antennas(-0.2, 0.6)),
];
const DANCE: Gesture =
    Gesture::new(&[DANCING, DANCING, &[step(0, 110, GLAD.stretch(-0.3)), spring(0, 260, Look::REST)]]).mirrorable();
const TILT: Gesture = Gesture::new(&[&[step(0, 200, PUZZLED), spring(800, 300, Look::REST)]]).mirrorable();
const CHECK: Gesture = Gesture::new(&[GLIMPSE, RELIEF]);
const DREAD: Gesture = Gesture::new(&[
    GLIMPSE,
    &[snap(450, 80, EEK.gaze(0.0, 1.0))],
    &tremble(EEK.gaze(0.0, 1.0)),
    &tremble(EEK.gaze(0.0, 1.0)),
    &[spring(0, 280, Look::REST)],
]);
const SNEEZE: Gesture = Gesture::new(&[&[
    step(0, 320, Look::REST.eyes(0.5, 0.5).mouth(1.3).head(0.0, -0.5).stretch(0.4).antennas(0.4, -0.4)),
    step(180, 160, Look::REST.eyes(0.35, 0.35).mouth(1.4).head(0.0, -0.8).stretch(0.6).antennas(0.6, -0.6)),
    fall(120, 70, Look::REST.eyes(0.0, 0.0).mouth(1.4).head(0.0, 1.0).stretch(-1.0).antennas(-0.5, 0.5)),
    spring(150, 300, Look::REST.eyes(0.5, 0.5)),
    fall(250, 70, SHUT),
    snap(40, 110, Look::REST),
]]);
const YAWN: Gesture = Gesture::new(&[&[
    step(0, 450, Look::REST.mouth(1.5).eyes(0.12, 0.12).stretch(0.8).head(0.0, -0.2).antennas(-0.6, 0.6)),
    step(500, 350, Look::REST.eyes(0.5, 0.5).stretch(-0.3)),
    spring(300, 260, Look::REST),
]]);
const NAP: Gesture = Gesture::new(&[DROOP, BREATHE, START]);
const WATCH: Gesture = Gesture::new(&[&[
    snap(0, 100, Look::REST.gaze(-0.9, -0.3).pupils(0.8).head(-0.3, -0.1)),
    steady(250, 160, Look::REST.gaze(-0.4, -0.85).pupils(0.8).head(-0.1, -0.3)),
    steady(0, 160, Look::REST.gaze(0.4, -0.85).pupils(0.8).head(0.1, -0.3)),
    steady(0, 160, Look::REST.gaze(0.9, -0.2).pupils(0.8).head(0.3, -0.1)),
    steady(0, 160, Look::REST.gaze(0.5, 0.7).pupils(0.8).head(0.2, 0.2)),
    fall(200, 70, SHUT.head(0.0, 0.3)),
    snap(80, 110, Look::REST),
]])
.mirrorable();
pub(super) const WINK: Gesture = Gesture::new(&[&[
    step(0, 120, BEAMING.eyes(0.0, 1.05).smile(0.8).lean(0.3).head(0.2, 0.0)),
    spring(380, 260, Look::REST),
]])
.mirrorable();
const HUM: Gesture = Gesture::new(&[&[
    step(0, 220, CONTENT.lean(0.4).antennas(-0.3, -0.3)),
    step(0, 300, CONTENT.lean(-0.4).antennas(0.3, 0.3)),
    step(0, 280, CONTENT.lean(0.3).antennas(-0.2, -0.2)),
    spring(0, 280, Look::REST),
]])
.mirrorable();
const GIGGLE: Gesture = Gesture::new(&[&[
    snap(0, 80, BEAMING),
    step(0, 70, BEAMING.stretch(0.25).antennas(0.25, -0.25)),
    step(0, 70, BEAMING.stretch(-0.15)),
    step(0, 70, BEAMING.stretch(0.2).antennas(0.2, -0.2)),
    step(0, 70, BEAMING.stretch(-0.1)),
    spring(80, 260, Look::REST),
]]);
const SIGH: Gesture = Gesture::new(&[&[
    step(0, 350, Look::REST.stretch(0.5).eyes(1.05, 1.05).mouth(0.6).head(0.0, -0.3)),
    step(150, 420, Look::REST.stretch(-0.5).eyes(0.55, 0.55).mouth(0.8).antennas(-0.4, 0.4)),
    spring(200, 300, Look::REST),
]]);
pub(super) const PEEK: Gesture = Gesture::new(&[&[
    snap(0, 110, Look::REST.gaze(-0.55, 0.83).pupils(0.85).head(-0.3, 0.4)),
    step(GLANCE_HOLD_MS, 180, Look::REST),
]])
.interruptible();
pub(super) const PEEK_ASIDE: Gesture = Gesture::new(&[&[
    snap(0, 110, Look::REST.gaze(-0.8, 0.6).pupils(0.85).head(-0.4, 0.3)),
    step(GLANCE_HOLD_MS, 180, Look::REST),
]])
.interruptible();
pub(super) const GREET: Gesture = Gesture::new(&[&[
    step(0, 110, GLAD.stretch(-1.0).antennas(-0.6, 0.6)),
    snap(0, 100, GLAD.stretch(0.6).head(0.0, -0.9).antennas(0.5, -0.5)),
    fall(40, 100, GLAD.stretch(-0.8).antennas(-0.7, 0.7)),
    spring(0, 280, GLAD),
    step(300, 220, Look::REST),
]]);
pub(super) const FLASH: Gesture = Gesture::new(&[&[
    snap(0, 60, SHUT.stretch(-0.4).mouth(0.5)),
    step(260, 110, Look::REST.eyes(0.0, 0.7).pupils(0.7)),
    step(220, 140, GLAD),
    spring(350, 260, Look::REST),
]]);
pub(super) const SLUMP: Gesture = Gesture::new(&[&[
    step(
        0,
        220,
        Look::REST
            .smile(-0.8)
            .eyes(0.6, 0.6)
            .gaze(0.0, 0.8)
            .pupils(0.85)
            .stretch(-0.6)
            .head(0.0, 0.5)
            .antennas(-0.6, 0.6),
    ),
    step(450, 320, Look::REST.smile(-0.5).eyes(0.7, 0.7).stretch(-0.25).antennas(-0.3, 0.3)),
    spring(300, 280, Look::REST),
]]);
pub(super) const PROUD: Gesture = Gesture::new(&[&[
    step(0, 110, GLAD.stretch(-0.6).antennas(-0.5, 0.5)),
    snap(0, 100, GLAD.stretch(0.5).head(0.0, -0.7).antennas(0.5, -0.5)),
    fall(0, 100, GLAD.stretch(-0.4).antennas(-0.6, 0.6)),
    step(0, 160, GLAD.lean(0.4).head(0.3, 0.0)),
    spring(250, 280, Look::REST),
]]);
pub(super) const ADMIRE: Gesture = Gesture::new(&[&[
    step(0, 170, Look::REST.gaze(0.0, 1.0).pupils(0.85).head(0.0, 0.3)),
    step(260, 160, Look::REST.gaze(-0.7, 0.6).pupils(0.85).head(-0.2, 0.3)),
    step(200, 120, GLAD),
    step(0, 80, GLAD.head(0.6, 0.0).antennas(-0.6, -0.6)),
    step(0, 90, GLAD.head(-0.6, 0.0).antennas(0.6, 0.6)),
    step(0, 90, GLAD.head(0.3, 0.0).antennas(-0.3, -0.3)),
    spring(0, 260, Look::REST),
]]);
pub(super) const BOOPED: Gesture = Gesture::new(&[&[
    snap(0, 70, Look::REST.eyes(0.0, 0.0).mouth(1.25).smile(0.9).stretch(-0.4).antennas(-0.4, 0.4)),
    step(0, 80, BEAMING.head(0.25, 0.0).stretch(-0.2)),
    step(0, 80, BEAMING.head(-0.25, 0.0)),
    step(0, 80, BEAMING.head(0.15, 0.0)),
    spring(60, 280, Look::REST),
]]);
pub(super) const LIFTED: Gesture = Gesture::new(&[&[
    snap(0, 80, SCARED.stretch(0.6).head(0.0, -0.3).antennas(-0.8, 0.8)),
    step(60, 140, WARY.gaze(0.0, 1.0).head(0.0, 0.2)),
    step(220, 160, GLAD),
    step(0, 130, GLAD.antennas(0.6, 0.6)),
    step(0, 150, GLAD.antennas(-0.5, -0.5)),
    step(0, 140, GLAD.antennas(0.3, 0.3)),
    spring(150, 280, Look::REST),
]]);
pub(super) const PERK: Gesture =
    Gesture::new(&[&[snap(0, 80, WARY.stretch(0.7).antennas(-0.6, 0.6)), spring(140, 280, Look::REST)]]);
const CLEARING: &[Step] = &[
    step(0, 160, Look::REST.stretch(0.5).mouth(0.3).head(0.0, -0.3).antennas(-0.3, 0.3)),
    spring(120, 260, Look::REST),
];
pub(super) const AHEM: Gesture = Gesture::new(&[CLEARING]);
pub(super) const HUSH: Gesture = Gesture::new(&[&[
    step(0, 120, Look::REST.mouth(0.0).eyes(0.7, 0.7).head(0.0, 0.3)),
    step(80, 220, Look::REST.mouth(0.0).antennas(-0.7, 0.7).gaze(0.0, 1.0).pupils(0.85).stretch(-0.3)),
    step(200, 280, Look::REST),
]]);
pub(super) const UNHUSH: Gesture = Gesture::new(&[CLEARING, &[step(60, 160, GLAD), spring(250, 260, Look::REST)]]);
pub(super) const FIRST_LIGHT: Gesture = Gesture::new(&[
    BLINK_AWAKE,
    &[
        snap(260, 120, WARY.gaze(-1.0, 0.0).head(-0.3, 0.0)),
        snap(320, 140, WARY.gaze(1.0, 0.0).head(0.3, 0.0)),
        step(280, 180, WARY),
        step(160, 200, GLAD),
        spring(260, 280, Look::REST),
    ],
]);
pub(super) const READY: Gesture = Gesture::new(&[&[
    step(0, 110, Look::REST.head(0.0, 0.7).eyes(0.8, 0.8)),
    snap(0, 120, Look::REST.head(0.0, -0.4).stretch(0.3)),
    spring(0, 260, Look::REST),
]])
.overlay();
pub(super) const ACTION: Gesture =
    Gesture::new(&[&[snap(0, 80, Look::REST.eyes(0.3, 1.1)), spring(100, 260, Look::REST)]]).overlay();
const BLINK_HELD: Gesture = Gesture::new(&[BLINKING]).overlay();
const PING: Gesture = Gesture::new(&[PINGING]).overlay();
const PING_BLINK: Gesture = Gesture::new(&[PINGING, BLINKING]).overlay();
const HARK_NOD: Gesture = Gesture::new(&[FOLLOWING]).overlay();
const SAY: Gesture = Gesture::new(&[&[
    step(0, 80, Look::REST.mouth(0.4)),
    step(0, 110, Look::REST.mouth(1.25).head(0.0, -0.15)),
    step(0, 80, Look::REST.mouth(0.5)),
    step(0, 100, Look::REST.mouth(1.1)),
    step(0, 80, Look::REST.mouth(0.35)),
    step(0, 110, Look::REST.mouth(1.2).antennas(0.2, -0.2)),
    step(0, 160, Look::REST),
]])
.overlay();
const SAY_SHORT: Gesture = Gesture::new(&[&[
    step(0, 80, Look::REST.mouth(0.4)),
    step(0, 110, Look::REST.mouth(1.2).head(0.0, -0.15)),
    step(0, 90, Look::REST.mouth(0.45)),
    step(0, 140, Look::REST),
]])
.overlay();
const SAY_ASK: Gesture = Gesture::new(&[&[
    step(0, 80, Look::REST.mouth(0.45)),
    step(0, 110, Look::REST.mouth(1.15)),
    step(0, 80, Look::REST.mouth(0.5)),
    step(0, 140, Look::REST.mouth(0.9).lean(0.7).eyes(1.08, 1.08)),
    spring(120, 260, Look::REST),
]])
.overlay()
.mirrorable();
const SAY_GLAD: Gesture = Gesture::new(&[&[
    step(0, 80, Look::REST.mouth(0.4)),
    step(0, 110, Look::REST.mouth(1.2).head(0.0, -0.2)),
    step(0, 80, Look::REST.mouth(0.5)),
    step(0, 140, Look::REST.mouth(1.15).smile(0.8).eyes(0.75, 0.75)),
    spring(150, 260, Look::REST),
]])
.overlay();
const MULL: Gesture =
    Gesture::new(&[&[step(0, 260, Look::REST.gaze(0.9, 0.0).head(0.4, -0.2)), step(500, 260, Look::REST)]]).overlay();
pub(super) const RIPPLE: Gesture = Gesture::new(&[&[steady(0, RIPPLE_TOTAL_MS, Look::REST.ripple(1.0))]]).overlay();
pub(super) const SCAN: Gesture = Gesture::new(&[&[
    step(0, 250, Look::REST.gaze(-1.0, 0.0).head(-0.6, 0.0).lean(-0.3).antennas(-0.4, -0.4)),
    step(150, 400, Look::REST.gaze(1.0, 0.0).head(0.6, 0.0).lean(0.3).antennas(0.4, 0.4)),
    spring(150, 250, Look::REST),
]])
.overlay()
.mirrorable();
const IDEA: Gesture = Gesture::new(&[&[
    snap(0, 70, Look::REST.antennas(0.0, 0.8).eyes(1.12, 1.12)),
    step(0, 120, Look::REST.antennas(0.0, -0.2).eyes(1.12, 1.12)),
    spring(220, 260, Look::REST),
]])
.overlay()
.mirrorable();
const HMM: Gesture = Gesture::new(&[HUMMING]).overlay();
const SQUINT: Gesture = Gesture::new(&[&[
    step(0, 120, Look::REST.eyes(0.5, 0.5).lean(-0.4).head(-0.3, 0.3)),
    spring(350, 240, Look::REST),
]])
.overlay();
const STUDY: Gesture = Gesture::new(&[&[
    step(0, 200, Look::REST.eyes(1.3, 1.3).gaze(0.8, 0.8).head(0.4, 0.6)),
    step(400, 220, Look::REST),
]])
.overlay();
pub(super) const STROKE: Gesture = Gesture::new(&[&[
    snap(0, 90, Look::REST.head(0.0, 1.0).stretch(-0.3)),
    step(0, 140, Look::REST.smile(0.6).head(0.0, -0.4)),
    spring(150, 240, Look::REST),
]])
.overlay();
const PEER: Gesture = Gesture::new(&[&[
    step(0, 160, Look::REST.eyes(0.5, 1.12).head(0.0, 0.6).stretch(-0.3)),
    spring(500, 280, Look::REST),
]])
.overlay();
pub(super) const POKED: [&Gesture; 6] = [&CHEER, &DANCE, &HOP, &BOUNCE, &WIGGLE, &GIGGLE];
pub(super) const FALL_ASLEEP: Gesture = Gesture::new(&[DROOP, BREATHE]);
const SNORE: Gesture = Gesture::new(&[BREATHE]);
const SNORT: Gesture = Gesture::new(&[&[
    step(0, 700, Look::ASLEEP.snore(0.4).stretch(0.0).mouth(0.7).head(0.1, 0.3)),
    snap(0, 160, Look::ASLEEP.snore(0.55).mouth(0.2).head(0.2, 0.6)),
    step(0, 140, Look::ASLEEP.snore(0.65).head(-0.05, 0.6)),
    steady(0, 1300, Look::ASLEEP.snore(1.0)),
]]);
const DREAM: Gesture = Gesture::new(&[&[
    step(0, 500, Look::ASLEEP.smile(1.0).stretch(-0.4).antennas(-0.2, 0.2)),
    step(700, 600, Look::ASLEEP),
]]);
const MUMBLE: Gesture = Gesture::new(&[&[
    step(0, 90, Look::ASLEEP.mouth(0.35)),
    step(0, 80, Look::ASLEEP.mouth(0.1)),
    step(0, 100, Look::ASLEEP.mouth(0.7).head(0.4, 0.5)),
    step(0, 80, Look::ASLEEP.mouth(0.05)),
    step(120, 90, Look::ASLEEP.mouth(0.3)),
    step(0, 200, Look::ASLEEP),
]]);
const DREAM_TWITCH: Gesture = Gesture::new(&[&[
    snap(0, 70, Look::ASLEEP.antennas(-0.8, 0.1)),
    step(0, 260, Look::ASLEEP),
    snap(180, 60, Look::ASLEEP.antennas(-0.8, 0.3)),
    spring(0, 280, Look::ASLEEP),
]]);
const SNUGGLE: Gesture = Gesture::new(&[&[
    step(0, 300, Look::ASLEEP.stretch(-0.75).head(0.3, 0.6).antennas(-0.2, 0.3)),
    step(0, 220, Look::ASLEEP.stretch(-0.7).head(-0.2, 0.6).antennas(-0.4, 0.4)),
    step(0, 220, Look::ASLEEP.stretch(-0.75).head(0.3, 0.6).antennas(-0.2, 0.3)),
    step(0, 400, Look::ASLEEP),
]]);
const PEEP: Gesture = Gesture::new(&[&[
    step(0, 260, Look::ASLEEP.eyes(0.0, 0.4).pupils(0.8)),
    step(150, 260, Look::ASLEEP.eyes(0.0, 0.4).pupils(0.8).gaze(0.8, 0.2)),
    step(200, 260, Look::ASLEEP.eyes(0.0, 0.4).pupils(0.8).gaze(-0.6, 0.2)),
    fall(250, 300, Look::ASLEEP),
]]);
pub(super) const SLEEPING: &[(&Gesture, u32)] =
    &[(&SNORE, 4), (&SNORT, 2), (&DREAM, 2), (&MUMBLE, 2), (&DREAM_TWITCH, 2), (&SNUGGLE, 1), (&PEEP, 1)];
pub(super) const WAKE: Gesture = Gesture::new(&[START]);
pub(super) const ROUSED: Gesture = Gesture::new(&[BLINK_AWAKE, GLIMPSE, REJOICE]);
pub(super) const PLUGGED: Gesture = Gesture::new(&[GLIMPSE, REJOICE]);
pub(super) const UNPLUGGED: Gesture = Gesture::new(&[GLIMPSE, SWALLOW]);
pub(super) const SATED: Gesture = Gesture::new(&[GLIMPSE, CONTENTED]);
pub(super) const IDLE: [(&Gesture, [u32; MOODS]); 30] = [
    (&BLINK, [6, 5, 6, 6, 5]),
    (&BLINK_TWICE, [2, 3, 1, 2, 1]),
    (&DART, [3, 6, 0, 1, 0]),
    (&LOOK_ASIDE, [2, 1, 1, 2, 0]),
    (&PONDER, [2, 0, 1, 2, 0]),
    (&WIGGLE, [3, 1, 0, 1, 0]),
    (&TWITCH, [2, 2, 1, 1, 0]),
    (&CHATTER, [2, 6, 0, 0, 0]),
    (&GULP, [1, 5, 0, 0, 0]),
    (&CHECK, [2, 0, 0, 0, 0]),
    (&DREAD, [0, 6, 0, 0, 0]),
    (&WATCH, [1, 0, 0, 0, 0]),
    (&NOD, [2, 0, 0, 1, 0]),
    (&SHAKE, [1, 2, 0, 0, 0]),
    (&STARTLE, [2, 3, 1, 0, 0]),
    (&HOP, [3, 0, 0, 0, 0]),
    (&BOUNCE, [4, 0, 0, 0, 0]),
    (&WOBBLE, [4, 1, 1, 0, 0]),
    (&SHIMMY, [3, 2, 0, 0, 0]),
    (&SWAY, [2, 0, 2, 0, 0]),
    (&CHEER, [3, 0, 0, 0, 0]),
    (&DANCE, [2, 0, 0, 0, 0]),
    (&TILT, [2, 1, 1, 2, 0]),
    (&SNEEZE, [1, 1, 1, 0, 0]),
    (&YAWN, [1, 0, 6, 1, 0]),
    (&NAP, [1, 0, 1, 0, 0]),
    (&WINK, [2, 0, 0, 1, 0]),
    (&HUM, [2, 0, 1, 1, 0]),
    (&GIGGLE, [2, 0, 0, 0, 0]),
    (&SIGH, [1, 1, 2, 1, 0]),
];
pub(super) const HARKING: &[(&Gesture, u32)] = &[(&PING, 4), (&PING_BLINK, 2), (&HARK_NOD, 1)];
pub(super) const SPEAKING: &[(&Gesture, u32)] =
    &[(&SAY, 4), (&SAY_SHORT, 3), (&SAY_ASK, 1), (&SAY_GLAD, 1), (&BLINK_HELD, 1)];
pub(super) const MUSING: &[(&Gesture, u32)] = &[(&RIPPLE, 3), (&MULL, 1), (&HMM, 1), (&IDEA, 1)];
pub(super) const SEEKING: &[(&Gesture, u32)] = &[(&SCAN, 1)];
pub(super) const PAINTING: &[(&Gesture, u32)] = &[(&BLINK_HELD, 1), (&SQUINT, 2), (&STUDY, 2)];
pub(super) const FILMING: &[(&Gesture, u32)] = &[(&BLINK_HELD, 1), (&PEER, 3)];
