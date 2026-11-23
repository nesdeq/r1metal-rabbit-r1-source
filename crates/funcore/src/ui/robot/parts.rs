use super::look::Look;
use super::{DRAWING, Easing, Piece, fraction, wave};
use crate::base::math::{short_sine_cosine, sqrt};
use crate::gfx::icon::{Icon, Point, extent};
use alloc::vec::Vec;

const HEAD: &str = "head";
const ANTENNAS: [&str; 2] = ["antenna-left", "antenna-right"];
const EYES: [&str; 2] = ["eye-left", "eye-right"];
const MOUTH: &str = "mouth";
const SHIFT: f32 = 1.0 / 32.0;
const LEAN: f32 = 0.1;
const STRETCH: f32 = 0.08;
const BREATH: f32 = 0.02;
const ANTENNA_BEND: f32 = 1.0 / 16.0;
const DIP: f32 = 2.0 / 3.0;
const EYE_SHUT: f32 = 0.14;
const MOUTH_SHUT: f32 = 0.25;
const MOUTH_BEND: f32 = 1.0 / 24.0;
const BEND_MEAN: f32 = 1.0 / 3.0;
const Z_COUNT: usize = 3;
const Z_GAP: f32 = 1.0 / 3.0;
const Z_OFFSET: f32 = 1.0 / 16.0;
pub(super) const Z_RISE: f32 = 0.3;
const Z_DRIFT: f32 = -0.21;
const Z_SWAY: f32 = 1.0 / 28.0;
const Z_WAVES: f32 = 1.5;
const Z_ROCK: f32 = 0.24;
pub(super) const Z_SIZE: f32 = 1.0 / 8.0;
const Z_SMALL: f32 = 0.4;
const Z_OUT: f32 = 0.1;
const Z_LINE: f32 = 0.24;
const Z_SLANT: f32 = 1.4;
const Z_CORNERS: usize = 10;
const RIPPLES: usize = 2;
const RIPPLE_MS: u32 = 900;
const RIPPLE_GAP_MS: u32 = 300;
pub(super) const RIPPLE_TOTAL_MS: u32 = RIPPLE_MS + (RIPPLES as u32 - 1) * RIPPLE_GAP_MS;

fn middle((low, high): (Point, Point)) -> Point {
    Point { x: (low.x + high.x) / 2.0, y: (low.y + high.y) / 2.0 }
}

fn bend(point: &Point, rise: f32, offset: f32, height: f32) -> Point {
    let share = (rise / height).max(0.0);
    let square = share * share;
    Point { x: point.x + offset * square, y: point.y + DIP * offset * offset / height * square * share }
}

fn ripple_ages(ripple: f32) -> impl Iterator<Item = f32> {
    let elapsed_ms = ripple * RIPPLE_TOTAL_MS as f32;
    (0..RIPPLES as u32)
        .map(move |ring| (elapsed_ms - (ring * RIPPLE_GAP_MS) as f32) / RIPPLE_MS as f32)
        .filter(|age| (0.0..1.0).contains(age) && *age != 0.0)
}

fn to_zed(at: Point, size: f32, (sine, cosine): (f32, f32)) -> [Point; Z_CORNERS] {
    let (half, line) = (size / 2.0, size * Z_LINE);
    let slant = line * Z_SLANT;
    [
        (-half, -half),
        (half, -half),
        (half, line - half),
        (slant - half, half - line),
        (half, half - line),
        (half, half),
        (-half, half),
        (-half, half - line),
        (half - slant, line - half),
        (-half, line - half),
    ]
    .map(|(x, y)| Point { x: at.x + x * cosine - y * sine, y: at.y + x * sine + y * cosine })
}

struct Outline {
    points: Vec<Point>,
    middle: Point,
    half: Point,
}

impl Outline {
    fn from_points(points: &[Point]) -> Outline {
        let (low, high) = extent(points);
        let half = Point { x: (high.x - low.x) / 2.0, y: (high.y - low.y) / 2.0 };
        Outline { points: points.to_vec(), middle: middle((low, high)), half }
    }
}

fn read_part(name: &str) -> (Vec<Outline>, (Point, Point)) {
    let icon = Icon::part(DRAWING, name).expect("the embedded drawing has the part");
    let extent = icon.extent().expect("a part of the drawing has outlines");
    (icon.contours().iter().map(|points| Outline::from_points(points)).collect(), extent)
}

fn count_points<'a>(outlines: impl IntoIterator<Item = &'a Outline>) -> usize {
    outlines.into_iter().map(|outline| outline.points.len()).sum()
}

struct Antenna {
    stalk: Outline,
    ball: Outline,
    foot: Point,
}

fn read_antenna(name: &str) -> Antenna {
    let (outlines, extent) = read_part(name);
    let Ok([stalk, ball]) = <[Outline; 2]>::try_from(outlines) else {
        panic!("an antenna of the drawing is a stalk and its ball");
    };
    Antenna { stalk, ball, foot: Point { x: middle(extent).x, y: extent.1.y } }
}

struct Eye {
    ring: Outline,
    hollow: Outline,
    pupil: Outline,
    middle: Point,
}

fn read_eye(name: &str) -> Eye {
    let (outlines, extent) = read_part(name);
    let Ok([ring, hollow, pupil]) = <[Outline; 3]>::try_from(outlines) else {
        panic!("an eye of the drawing is a ring, its hollow and a pupil");
    };
    Eye { ring, hollow, pupil, middle: middle(extent) }
}

struct Mouth {
    lips: Outline,
    gaps: Vec<Outline>,
    middle: Point,
}

fn read_mouth(name: &str) -> Mouth {
    let (outlines, extent) = read_part(name);
    let mut outlines = outlines.into_iter();
    let lips = outlines.next().expect("the mouth of the drawing has its lips");
    Mouth { lips, gaps: outlines.collect(), middle: middle(extent) }
}

#[derive(Clone, Copy)]
struct Pose {
    centre: Point,
    foot: f32,
    stretch: f32,
    sine: f32,
    cosine: f32,
    shift: (f32, f32),
}

impl Pose {
    fn apply(&self, point: Point) -> Point {
        let x = (point.x - self.centre.x) / self.stretch;
        let y = self.foot - (self.foot - point.y) * self.stretch - self.centre.y;
        Point {
            x: self.centre.x + x * self.cosine - y * self.sine + SHIFT * self.shift.0,
            y: self.centre.y + x * self.sine + y * self.cosine + SHIFT * self.shift.1,
        }
    }
}

pub(super) struct Parts {
    head: Vec<Outline>,
    antennas: [Antenna; 2],
    eyes: [Eye; 2],
    mouth: Mouth,
    pub(super) extent: (Point, Point),
    line: f32,
    reach: f32,
}

impl Parts {
    pub(super) fn new() -> Parts {
        let drawing = Icon::from_svg(DRAWING).ok();
        let extent = drawing.and_then(|icon| icon.extent()).expect("the embedded drawing is valid");
        let head = read_part(HEAD).0;
        let antennas = ANTENNAS.map(read_antenna);
        let line = 2.0 * antennas[0].stalk.half.x;
        let mut reach = f32::MAX;
        for antenna in &antennas {
            for point in head.iter().flat_map(|outline| outline.points.iter()) {
                let (x, y) = (point.x - antenna.ball.middle.x, point.y - antenna.ball.middle.y);
                reach = reach.min(sqrt(x * x + y * y));
            }
        }
        Parts { head, antennas, eyes: EYES.map(read_eye), mouth: read_mouth(MOUTH), extent, line, reach }
    }

    pub(super) fn crown(&self) -> f32 {
        self.head.first().map_or(self.extent.0.y, |head| head.middle.y - head.half.y)
    }

    pub(super) fn points(&self) -> usize {
        let balls = count_points(self.antennas.iter().map(|antenna| &antenna.ball));
        let stalks = count_points(self.antennas.iter().map(|antenna| &antenna.stalk));
        let eyes = count_points(self.eyes.iter().flat_map(|eye| [&eye.ring, &eye.hollow, &eye.pupil]));
        let mouth = count_points(core::iter::once(&self.mouth.lips).chain(&self.mouth.gaps));
        count_points(&self.head) + stalks + balls + eyes + mouth + RIPPLES * 2 * balls + Z_COUNT * Z_CORNERS
    }

    fn pose_of(&self, look: &Look, breath: f32) -> Pose {
        let (sine, cosine) = short_sine_cosine(LEAN * look.lean);
        Pose {
            centre: middle(self.extent),
            foot: self.extent.1.y,
            stretch: 1.0 + STRETCH * look.stretch + BREATH * wave(breath),
            sine,
            cosine,
            shift: look.head,
        }
    }

    pub(super) fn outlines(
        &self,
        look: &Look,
        z_rise: f32,
        points: &mut Vec<Point>,
        mut visit: impl FnMut(Piece, &[Point]),
    ) {
        let mut emit = |piece: Piece, outline: &mut dyn Iterator<Item = Point>| {
            points.clear();
            points.extend(outline);
            visit(piece, points);
        };
        let breath = fraction(look.snore);
        let pose = self.pose_of(look, breath);
        for outline in &self.head {
            emit(Piece::Body, &mut outline.points.iter().map(|point| pose.apply(*point)));
        }
        let mut bends = [(0.0, 1.0); 2];
        for (index, antenna) in self.antennas.iter().enumerate() {
            let offset = ANTENNA_BEND * look.antennas[index].clamp(-1.0, 1.0);
            let height = antenna.foot.y - antenna.ball.middle.y;
            bends[index] = (offset, height);
            let foot = antenna.foot.y;
            let stalk =
                antenna.stalk.points.iter().map(|point| pose.apply(bend(point, foot - point.y, offset, height)));
            emit(Piece::Body, &mut { stalk });
            emit(
                Piece::Tip,
                &mut antenna.ball.points.iter().map(|point| pose.apply(bend(point, height, offset, height))),
            );
        }
        let gaze_square = look.gaze.0 * look.gaze.0 + look.gaze.1 * look.gaze.1;
        let gaze_length = if gaze_square > 1.0 { sqrt(gaze_square) } else { 1.0 };
        let gaze = (look.gaze.0 / gaze_length, look.gaze.1 / gaze_length);
        for (eye, open) in self.eyes.iter().zip(look.eyes) {
            let (widen, shut) = (open.max(1.0), open.clamp(0.0, 1.0));
            let scale_eye = |point: &Point, height: f32| {
                pose.apply(Point {
                    x: eye.middle.x + (point.x - eye.middle.x) * widen,
                    y: eye.middle.y + (point.y - eye.middle.y) * widen * height,
                })
            };
            let ring_height = EYE_SHUT + (1.0 - EYE_SHUT) * shut;
            emit(Piece::Body, &mut eye.ring.points.iter().map(|point| scale_eye(point, ring_height)));
            emit(Piece::Body, &mut eye.hollow.points.iter().map(|point| scale_eye(point, shut)));
            let size = look.pupils.max(0.0);
            let room = (eye.hollow.half.x * widen - eye.pupil.half.x * size).max(0.0);
            let place_pupil = |point: &Point| {
                let x = room * gaze.0 + (point.x - eye.pupil.middle.x) * size;
                let y = room * gaze.1 + (point.y - eye.pupil.middle.y) * size;
                pose.apply(Point { x: eye.middle.x + x, y: eye.middle.y + y * shut })
            };
            emit(Piece::Body, &mut eye.pupil.points.iter().map(place_pupil));
        }
        let open = look.mouth.max(0.0);
        let lips = &self.mouth.lips;
        let shape_mouth = |point: &Point, height: f32| {
            let x = (point.x - self.mouth.middle.x) / lips.half.x;
            let rise = MOUTH_BEND * look.smile * (x * x - BEND_MEAN);
            pose.apply(Point { x: point.x, y: self.mouth.middle.y + (point.y - self.mouth.middle.y) * height - rise })
        };
        let lips_height = MOUTH_SHUT + (1.0 - MOUTH_SHUT) * open;
        emit(Piece::Body, &mut lips.points.iter().map(|point| shape_mouth(point, lips_height)));
        for gap in &self.mouth.gaps {
            emit(Piece::Body, &mut gap.points.iter().map(|point| shape_mouth(point, open)));
        }
        for age in ripple_ages(look.ripple) {
            for (antenna, &(offset, height)) in self.antennas.iter().zip(&bends) {
                let ball = &antenna.ball;
                let outer = ball.half.x + (self.reach - ball.half.x) * Easing::Snap.share(age);
                let inner = (outer - self.line * (1.0 - age) * (age / Z_OUT).min(1.0)).max(0.0);
                let scale_ball = |radius: f32| {
                    let scale = radius / ball.half.x;
                    move |point: &Point| {
                        let at = Point {
                            x: ball.middle.x + (point.x - ball.middle.x) * scale,
                            y: ball.middle.y + (point.y - ball.middle.y) * scale,
                        };
                        pose.apply(bend(&at, height, offset, height))
                    }
                };
                emit(Piece::Body, &mut ball.points.iter().map(scale_ball(outer)));
                emit(Piece::Body, &mut ball.points.iter().rev().map(scale_ball(inner)));
            }
        }
        let start = pose.apply(Point { x: pose.centre.x + Z_OFFSET, y: self.crown() });
        for number in 0..Z_COUNT {
            let age = breath * (1.0 + (Z_COUNT - 1) as f32 * Z_GAP) - number as f32 * Z_GAP;
            if age <= 0.0 || age >= 1.0 {
                continue;
            }
            let size = Z_SIZE * (Z_SMALL + (1.0 - Z_SMALL) * age) * (age / Z_OUT).min(1.0);
            let sway = wave(Z_WAVES * age + number as f32 * Z_GAP);
            let at = Point { x: start.x + Z_DRIFT * age + Z_SWAY * sway, y: start.y - z_rise * age };
            emit(Piece::Body, &mut to_zed(at, size, short_sine_cosine(Z_ROCK * sway)).into_iter());
        }
    }
}
