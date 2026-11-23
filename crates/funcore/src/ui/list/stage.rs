use crate::gfx::Rect;
use crate::gfx::icon::{Icon, Point};
use crate::gfx::mask::Mask;
use crate::gfx::measures::PHI2;
use crate::gfx::motion::{EXIT_BOXES, Edge, GIVE, PULL, Pace, RELEASE_SHARE, Spring, Swing, spring};
use crate::gfx::palette::Roles;
use crate::gfx::pen::Pen;
use crate::ui::field::draw_field;
use crate::ui::kit::Kit;
use alloc::vec::Vec;

const DOCK_SHARE: f32 = 1.0 / PHI2;
const BULGE: f32 = 1.5;
const PULL_SHARE: f32 = 0.6;
const PAUSE_NS: u64 = 150_000_000;
const PRESS_SHARE: f32 = 1.0 / 12.0;
const NECK: f32 = 0.75;
const FLATTEST: f32 = 0.4;
const FREE: Spring = spring(28.0, 0.6);
const TRAILING: Spring = spring(50.0, 0.3);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Way {
    Up,
    Down,
}

impl Way {
    const fn sign(self) -> f32 {
        match self {
            Way::Up => -1.0,
            Way::Down => 1.0,
        }
    }

    fn from_sign(sign: f32) -> Way {
        if sign < 0.0 { Way::Up } else { Way::Down }
    }
}

#[derive(Clone, Copy)]
struct Seat {
    top: f32,
    bottom: f32,
    middle: f32,
    breadth: f32,
}

impl Seat {
    const fn front_and_rear(self, way: Way) -> (f32, f32) {
        match way {
            Way::Up => (self.top, self.bottom),
            Way::Down => (self.bottom, self.top),
        }
    }
}

struct Flyer {
    icon: usize,
    swing: Swing,
    middle: Edge,
    breadth: Edge,
    is_leaving: bool,
}

impl Flyer {
    fn at_rest(icon: usize, seat: Seat) -> Flyer {
        Flyer {
            icon,
            swing: Swing::at_rest(seat.top, seat.bottom, Way::Up.sign()),
            middle: Edge::still(seat.middle),
            breadth: Edge::still(seat.breadth),
            is_leaving: false,
        }
    }

    fn head(&mut self, way: Way) {
        self.swing.way = way.sign();
        self.swing.has_landed = false;
        self.swing.has_swung = false;
    }
}

pub(in crate::ui) struct Stage {
    icons: Vec<Icon>,
    width: f32,
    height: f32,
    side: f32,
    flyers: Vec<Flyer>,
    pub(in crate::ui) selection: usize,
    pull: Option<Way>,
    turn_ns: u64,
    done_way: Option<Way>,
    pending_way: Option<Way>,
    is_pressed: bool,
    is_docked: bool,
    lift_places: usize,
    pub(in crate::ui) is_resting: bool,
    pace: Pace,
}

impl Stage {
    pub(in crate::ui) fn new(width: i32, height: i32, side: f32, icons: Vec<Icon>) -> Stage {
        Stage {
            icons,
            width: width.max(0) as f32,
            height: height.max(0) as f32,
            side,
            flyers: Vec::new(),
            selection: 0,
            pull: None,
            turn_ns: 0,
            done_way: None,
            pending_way: None,
            is_pressed: false,
            is_docked: false,
            lift_places: 0,
            is_resting: true,
            pace: Pace::default(),
        }
    }

    pub(in crate::ui) fn lift(&mut self, places: usize, now_ns: u64) {
        self.advance(now_ns);
        self.is_resting &= self.lift_places == places;
        self.lift_places = places;
    }

    pub(in crate::ui) fn replace(&mut self, index: usize, icon: Icon) {
        if let Some(existing) = self.icons.get_mut(index) {
            *existing = icon;
        }
    }

    // Takes the icons of the nodes that show as they change, each icon that stays where it stands now; where the one shown goes, the next comes in its place.
    pub(in crate::ui) fn reindex(&mut self, icons: Vec<Icon>, to_new: &dyn Fn(usize) -> Option<usize>) {
        let is_gone = self.flyers.iter().any(|flyer| !flyer.is_leaving && to_new(flyer.icon).is_none());
        self.selection = to_new(self.selection).unwrap_or(self.selection).min(icons.len().saturating_sub(1));
        self.icons = icons;
        self.flyers.retain_mut(|flyer| match to_new(flyer.icon) {
            Some(icon) => {
                flyer.icon = icon;
                true
            }
            None => false,
        });
        if is_gone {
            self.is_resting = false;
            self.bring_in();
        }
    }

    fn dock_side(&self) -> f32 {
        self.side * DOCK_SHARE
    }

    fn dock_top(&self) -> f32 {
        (self.height - self.dock_side()) / 2.0 - self.lift_places as f32 * self.dock_side()
    }

    fn lead(&self) -> Option<&Flyer> {
        self.flyers.iter().find(|flyer| !flyer.is_leaving)
    }

    fn lead_mut(&mut self) -> Option<&mut Flyer> {
        self.flyers.iter_mut().find(|flyer| !flyer.is_leaving)
    }

    fn arrow_place(&self) -> Option<(f32, f32, f32)> {
        let flyer = self.lead().filter(|_| self.is_docked)?;
        let side = self.dock_side();
        Some((((self.height - side) / 2.0 - flyer.swing.near.at) / side.max(1.0), flyer.middle.at, flyer.swing.far.at))
    }

    pub(in crate::ui) fn rest_edges(&self) -> Option<(f32, f32)> {
        let flyer = self.flyers.iter().find(|flyer| !flyer.is_leaving && flyer.swing.has_landed)?;
        (!self.is_docked).then_some((flyer.swing.near.at, flyer.swing.far.at))
    }

    pub(in crate::ui) fn rewind(&mut self, now_ns: u64) {
        self.advance(now_ns);
        let is_home = self.selection == 0 && !self.is_docked && self.lead().is_some();
        self.loosen(false, false);
        if is_home {
            return;
        }
        self.selection = 0;
        self.fall_off();
        let mut flyer = Flyer::at_rest(0, self.seat());
        flyer.swing.near.at = -self.side;
        flyer.swing.far.at = 0.0;
        flyer.head(Way::Down);
        self.flyers.push(flyer);
    }

    pub(in crate::ui) fn stand(&mut self, index: usize, is_docked: bool) {
        self.loosen(is_docked, true);
        self.selection = index.min(self.icons.len().saturating_sub(1));
        self.flyers.clear();
        self.flyers.push(Flyer::at_rest(self.selection, self.seat()));
    }

    // Takes every icon off the screen at once, where `leave` lets them fall.
    pub(in crate::ui) fn clear(&mut self) {
        self.loosen(false, true);
        self.flyers.clear();
    }

    const fn loosen(&mut self, is_docked: bool, is_resting: bool) {
        self.is_docked = is_docked;
        self.is_pressed = false;
        self.pull = None;
        self.is_resting = is_resting;
        self.lift_places = 0;
        self.done_way = None;
        self.pending_way = None;
    }

    fn is_empty(&self) -> bool {
        self.flyers.is_empty()
    }

    fn count(&self) -> usize {
        self.icons.len()
    }

    pub(in crate::ui) fn feet(&self) -> impl Iterator<Item = (usize, f32, f32)> + '_ {
        let flyers = self.flyers.iter().filter(|_| !self.is_docked);
        flyers.map(|flyer| (flyer.icon, flyer.swing.near.at.max(flyer.swing.far.at), flyer.middle.at))
    }

    fn seat(&self) -> Seat {
        if self.is_docked {
            let (side, top) = (self.dock_side(), self.dock_top());
            return Seat { top, bottom: top + side, middle: side / 2.0, breadth: side };
        }
        let half = self.side / 2.0 - if self.is_pressed { self.side * PRESS_SHARE } else { 0.0 };
        Seat {
            top: self.height / 2.0 - half,
            bottom: self.height / 2.0 + half,
            middle: self.width / 2.0,
            breadth: self.side,
        }
    }

    // Turns the wheel, downward for positive `detents`.
    pub(in crate::ui) fn turn(&mut self, detents: i32, now_ns: u64) {
        self.advance(now_ns);
        let has_paused = now_ns.saturating_sub(core::mem::replace(&mut self.turn_ns, now_ns)) >= PAUSE_NS;
        if detents == 0 || self.is_docked {
            return;
        }
        let has_landed = self.lead().is_some_and(|flyer| flyer.swing.has_landed);
        let way = if detents < 0 { Way::Up } else { Way::Down };
        if let Some(pending) = self.pending_way {
            self.pending_way = (pending == way).then_some(pending);
            return;
        }
        if let Some(done) = self.done_way {
            let is_new_spin = has_paused && has_landed;
            if !is_new_spin && done == way {
                return;
            }
            if !is_new_spin && !has_landed {
                self.pending_way = Some(way);
                return;
            }
            self.done_way = None;
        }
        if !has_landed {
            return;
        }
        self.pull = if self.pull.is_some_and(|pull| pull != way) { None } else { Some(way) };
        self.is_resting = false;
        if let Some(pull) = self.pull
            && let Some(flyer) = self.lead_mut()
        {
            flyer.swing.way = pull.sign();
        }
    }

    pub(in crate::ui) fn press(&mut self, is_down: bool, now_ns: u64) {
        self.advance(now_ns);
        self.is_resting &= self.is_pressed == is_down;
        self.is_pressed = is_down;
    }

    pub(in crate::ui) fn tuck(&mut self, is_docked: bool, now_ns: u64) {
        self.advance(now_ns);
        self.is_docked = is_docked;
        self.is_pressed = false;
        self.pull = None;
        self.is_resting = false;
        self.lift_places = 0;
        self.pending_way = None;
        if let Some(flyer) = self.lead_mut() {
            flyer.head(if is_docked { Way::Up } else { Way::Down });
        }
    }

    pub(in crate::ui) fn enter(&mut self, now_ns: u64) {
        self.loosen(false, false);
        self.selection = 0;
        self.pace.begin(now_ns);
        self.flyers.clear();
        self.bring_in();
    }

    fn bring_in(&mut self) {
        let mut flyer = Flyer::at_rest(self.selection, self.seat());
        flyer.swing.near.at = self.height;
        flyer.swing.far.at = self.height + self.side;
        flyer.head(Way::Up);
        self.flyers.push(flyer);
    }

    pub(in crate::ui) fn show(&mut self, index: usize, now_ns: u64) {
        self.advance(now_ns);
        if index >= self.icons.len() || (index == self.selection && !self.is_docked && self.lead().is_some()) {
            return;
        }
        self.loosen(false, false);
        self.selection = index;
        for flyer in &mut self.flyers {
            flyer.is_leaving = true;
            flyer.swing.way = Way::Up.sign();
        }
        self.bring_in();
    }

    fn fall_off(&mut self) {
        for flyer in self.flyers.iter_mut().filter(|flyer| !flyer.is_leaving) {
            flyer.is_leaving = true;
            flyer.swing.way = Way::Down.sign();
        }
    }

    pub(in crate::ui) fn leave(&mut self, now_ns: u64) {
        self.advance(now_ns);
        self.is_pressed = false;
        self.pull = None;
        self.pending_way = None;
        self.is_resting = false;
        self.fall_off();
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64) {
        for _ in 0..self.pace.take_ticks(now_ns, self.is_resting) {
            if self.is_resting {
                break;
            }
            self.step();
        }
    }

    fn step_leaver(flyer: &mut Flyer, side: f32, height: f32) {
        let sign = flyer.swing.way;
        let exit = if sign < 0.0 { -side * EXIT_BOXES } else { height + side * EXIT_BOXES };
        flyer.swing.step_out(exit, sign * flyer.breadth.at, TRAILING);
    }

    fn step_seated(flyer: &mut Flyer, seat: Seat, pull: f32) -> (Option<f32>, bool) {
        let sign = flyer.swing.way;
        let (front_seat, rear_seat) = seat.front_and_rear(Way::from_sign(sign));
        flyer.middle.follow(seat.middle, FREE);
        flyer.breadth.follow(seat.breadth, FREE);
        let is_side_still = flyer.middle.is_at_rest(seat.middle) && flyer.breadth.is_at_rest(seat.breadth);
        let goals = (front_seat + pull, rear_seat + GIVE * pull);
        let spring = if pull == 0.0 { FREE } else { PULL };
        let is_past = flyer.swing.step_edges(goals, spring, (sign * (rear_seat - front_seat).abs(), TRAILING));
        let front = flyer.swing.front();
        let release =
            (pull != 0.0 && (front.at - front_seat) * sign >= RELEASE_SHARE * pull.abs()).then_some(front.speed);
        let is_still = flyer.swing.step_rest(goals, is_past, is_side_still);
        if is_still {
            flyer.middle = Edge::still(seat.middle);
            flyer.breadth = Edge::still(seat.breadth);
        }
        (release, is_still)
    }

    fn step(&mut self) {
        let (side, height) = (self.side, self.height);
        if let Some(pending) = self.pending_way
            && let Some(flyer) = self.lead_mut().filter(|flyer| flyer.swing.has_landed)
        {
            flyer.swing.way = pending.sign();
            self.pull = Some(pending);
            self.pending_way = None;
            self.done_way = None;
        }
        let seat = self.seat();
        let pull = self.pull.map_or(0.0, |way| way.sign() * side * PULL_SHARE);
        let mut release = None;
        let mut is_moving = false;
        for flyer in &mut self.flyers {
            if flyer.is_leaving {
                Stage::step_leaver(flyer, side, height);
                is_moving = true;
                continue;
            }
            let way = Way::from_sign(flyer.swing.way);
            let (speed, is_still) = Stage::step_seated(flyer, seat, pull);
            if let Some(speed) = speed {
                release = Some((way, speed));
            }
            is_moving |= !is_still;
        }
        if let Some((way, speed)) = release {
            self.let_go(way, speed);
        }
        self.flyers.retain(|flyer| !flyer.is_leaving || (flyer.swing.far.at > 0.0 && flyer.swing.near.at < height));
        self.is_resting = !is_moving && release.is_none() && self.pending_way.is_none();
    }

    fn let_go(&mut self, way: Way, speed: f32) {
        let count = self.icons.len().max(1);
        self.selection = match way {
            Way::Up => (self.selection + 1) % count,
            Way::Down => (self.selection + count - 1) % count,
        };
        self.pull = None;
        self.done_way = Some(way);
        for flyer in &mut self.flyers {
            flyer.is_leaving = true;
        }
        let top = if way == Way::Up { self.height } else { -self.side };
        let mut flyer = Flyer::at_rest(self.selection, self.seat());
        flyer.swing.near = Edge { at: top, speed };
        flyer.swing.far = Edge { at: top + self.side, speed };
        flyer.head(way);
        self.flyers.push(flyer);
    }

    fn trace(&self, mask: &mut Mask, is_traced: impl Fn(usize) -> bool) {
        for flyer in self.flyers.iter().filter(|flyer| is_traced(flyer.icon)) {
            let breadth = flyer.breadth.at;
            let Some(icon) = self.icons.get(flyer.icon).filter(|_| breadth > 0.0) else {
                continue;
            };
            let (near, far) = (flyer.swing.near.at, flyer.swing.far.at);
            let height = (far - near).max(breadth * FLATTEST);
            let top = (near + far - height) / 2.0;
            let neck = (NECK * (1.0 - breadth / height)).max(1.0 - BULGE);
            let to_screen = |point: &Point| {
                let width = breadth * (1.0 - neck * 4.0 * point.y * (1.0 - point.y));
                (flyer.middle.at + (point.x - 0.5) * width, top + point.y * height)
            };
            for contour in icon.contours() {
                mask.outline(contour.iter().map(to_screen));
            }
        }
    }

    pub(in crate::ui) fn draw(&self, pen: &mut Pen<'_, '_>, roles: &Roles, robot: Option<usize>) {
        if self.is_empty() {
            return;
        }
        pen.cover(roles.text, roles.background, |mask| self.trace(mask, |icon| Some(icon) != robot));
        if robot.is_some() {
            pen.cover(roles.robot, roles.background, |mask| self.trace(mask, |icon| Some(icon) == robot));
        }
    }

    pub(in crate::ui) fn draw_dots(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>) {
        let count = self.count() as i32;
        if count < 2 {
            return;
        }
        let (ruler, roles) = (kit.ruler, kit.roles);
        let (small, large) = ruler.lines();
        let step = ruler.gap + large;
        let (x, top) = (ruler.width - ruler.gap - large / 2, ruler.height / 2 - count * step / 2);
        for place in 0..count {
            let is_selected = place as usize == self.selection;
            let (side, colour) = if is_selected { (large, roles.text) } else { (small, roles.rule) };
            pen.fill(Rect::new(x - side / 2, top + place * step - side / 2, side, side), colour);
        }
    }
}

fn trace_arrow(mask: &mut Mask, (x, y): (f32, f32), width: f32, height: f32, line: f32) {
    let (half, top, tip) = (width / 2.0, y - height / 2.0, y + height / 2.0);
    let inset = half * line / height.max(1.0);
    let outline =
        [(x - half, top), (x, tip), (x + half, top), (x + half - inset, top), (x, tip - line), (x - half + inset, top)];
    mask.outline(outline.into_iter());
}

pub(in crate::ui) fn draw_arrows<'a>(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, docked: impl Iterator<Item = &'a Stage>) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (width, height, line) = (ruler.rim(), ruler.gap as f32, ruler.lines().0 as f32);
    pen.cover(roles.text_dim, roles.background, |mask| {
        for (places, x, foot) in docked.filter_map(Stage::arrow_place) {
            let open = places.clamp(0.0, 1.0);
            trace_arrow(mask, (x, foot), width * open, height * open, line * open);
        }
    });
}

pub(in crate::ui) fn draw_name(
    pen: &mut Pen<'_, '_>,
    kit: &Kit<'_>,
    (centre, foot): (i32, i32),
    name: &str,
    (word, cue): (&str, Option<&str>),
) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let top = foot + ruler.gap;
    let left = centre - ruler.heading.text_width(name) / 2;
    pen.text(None, &ruler.heading, (left, top), name, roles.text, 1);
    let mut draw_centred = |text: &str, y: i32| {
        let room = ruler.body.text_width(text).min(ruler.pane_width() - 2 * ruler.gap);
        draw_field(kit, pen, &ruler.body, (centre - room / 2, y), text, room, roles.text_dim);
    };
    draw_centred(word, top + ruler.large());
    if let Some(cue) = cue {
        draw_centred(cue, top + ruler.large() + ruler.small());
    }
}
