use crate::base::math::round;
use crate::gfx::key::Ids;
use crate::gfx::motion::{
    Clock, EXIT_BOXES, Edge, GIVE, NUDGE_SPEED, PULL, RELEASE_SHARE, SWING, Spring, Swing, spring,
};
use crate::gfx::pen::Pen;
use crate::gfx::{Picture, Rect};
use alloc::vec::Vec;
use core::cmp::Ordering;

const FOLLOWING: Spring = spring(160.0, 1.0);

#[derive(Clone, Copy)]
struct Pane {
    swing: Swing,
    length: f32,
}

impl Pane {
    const fn at_rest(length: f32) -> Pane {
        Pane { swing: Swing::at_rest(0.0, 0.0, 1.0), length }
    }
}

struct Departure {
    pane: Pane,
    picture: Picture,
    id: u64,
    item: usize,
    is_flipped: bool,
    is_leaving: bool,
}

pub(in crate::ui) struct Carousel {
    pull: f32,
    departures: Vec<Departure>,
    lead: Option<Pane>,
    next_length: Option<f32>,
    clock: Clock,
}

impl Carousel {
    pub(in crate::ui) fn new(pull: f32) -> Carousel {
        Carousel { pull, departures: Vec::new(), lead: None, next_length: None, clock: Clock::new() }
    }

    pub(in crate::ui) fn clear(&mut self) {
        self.departures.clear();
        self.lead = None;
        self.next_length = None;
        self.clock.halt();
    }

    pub(in crate::ui) fn rest(&mut self) {
        self.departures.clear();
        self.lead = self.lead.map(|lead| Pane::at_rest(lead.length));
        self.next_length = None;
        self.clock.halt();
    }

    pub(in crate::ui) fn has_panes(&self) -> bool {
        self.lead.is_some() || !self.departures.is_empty()
    }

    pub(in crate::ui) const fn is_resting(&self) -> bool {
        self.clock.is_resting()
    }

    fn pulled(&mut self) -> Option<&mut Departure> {
        self.departures.last_mut().filter(|departure| !departure.is_leaving)
    }

    pub(in crate::ui) fn show(&mut self, length: f32) {
        if self.pulled().is_some() {
            self.next_length = Some(length);
            self.clock.stir();
        } else if self.lead.is_none_or(|lead| lead.length != length) {
            self.lead = Some(Pane::at_rest(length));
        }
    }

    pub(in crate::ui) fn pull(&mut self, ids: &Ids, target: usize, current: Option<(usize, Picture)>) {
        self.hold(ids, current, false);
        if let Some(departure) = self.pulled() {
            departure.pane.swing.way = match target.cmp(&departure.item) {
                Ordering::Greater => -1.0,
                Ordering::Less => 1.0,
                Ordering::Equal => departure.pane.swing.way,
            };
            self.clock.stir();
        }
    }

    pub(in crate::ui) fn flip(&mut self, ids: &Ids, current: Option<(usize, Picture)>, way: f32) {
        self.hold(ids, current, true);
        if let Some(departure) = self.pulled() {
            departure.pane.swing.way = way;
            self.clock.stir();
        }
    }

    fn hold(&mut self, ids: &Ids, current: Option<(usize, Picture)>, is_flipped: bool) {
        if let (Some((item, picture)), Some(mut pane)) = (current, self.lead.take()) {
            pane.swing.has_landed = true;
            self.departures.push(Departure { pane, picture, id: ids.next(), item, is_flipped, is_leaving: false });
        }
        self.next_length = None;
    }

    pub(in crate::ui) fn take_back(&mut self, item: usize) -> Option<Picture> {
        let departure = self.departures.pop_if(|last| !last.is_leaving && !last.is_flipped && last.item == item)?;
        self.lead = Some(departure.pane);
        self.next_length = None;
        self.clock.stir();
        Some(departure.picture)
    }

    pub(in crate::ui) fn bump(&mut self, way: f32) {
        if let Some(lead) = &mut self.lead {
            lead.swing.way = way;
            lead.swing.has_landed = true;
            lead.swing.front_and_rear().0.speed += way * NUDGE_SPEED;
            self.clock.stir();
        }
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64, room: f32) {
        let mut clock = core::mem::take(&mut self.clock);
        clock.run(now_ns, || self.step(room));
        self.clock = clock;
    }

    fn step_departures(&mut self, room: f32) -> (bool, Option<f32>) {
        let (pull, has_next) = (self.pull, self.next_length.is_some());
        let (mut is_moving, mut release) = (false, None);
        for departure in &mut self.departures {
            let (swing, length) = (&mut departure.pane.swing, departure.pane.length);
            let way = swing.way;
            if departure.is_leaving {
                swing.step_out(way * ((room - length) / 2.0 + length * EXIT_BOXES), 0.0, FOLLOWING);
                is_moving = true;
                continue;
            }
            let goals = (way * pull, way * GIVE * pull);
            let is_past = swing.step_edges(goals, PULL, (0.0, FOLLOWING));
            let front = swing.front();
            if has_next && front.at * way >= RELEASE_SHARE * pull {
                release = Some(front.speed);
            }
            is_moving |= !swing.step_rest(goals, is_past, true);
        }
        (is_moving, release)
    }

    fn step_lead(&mut self) -> bool {
        let Some(pane) = &mut self.lead else {
            return false;
        };
        let is_past = pane.swing.step_edges((0.0, 0.0), SWING, (0.0, FOLLOWING));
        !pane.swing.step_rest((0.0, 0.0), is_past, true)
    }

    fn push_forward(&mut self) {
        let mut pusher = self.lead.map(|lead| (lead.swing.way, lead.swing.front(), lead.length));
        for departure in self.departures.iter_mut().rev() {
            let swing = &mut departure.pane.swing;
            if let Some((way, front, length)) = pusher.filter(|pusher| departure.is_leaving && swing.way == pusher.0) {
                let overlap = way * (front.at - swing.rear().at) + (length + departure.pane.length) / 2.0;
                if overlap > 0.0 {
                    for edge in [&mut swing.near, &mut swing.far] {
                        edge.at += way * overlap;
                        if (front.speed - edge.speed) * way > 0.0 {
                            edge.speed = front.speed;
                        }
                    }
                }
            }
            pusher = Some((swing.way, swing.front(), departure.pane.length));
        }
    }

    fn step(&mut self, room: f32) -> bool {
        let (mut is_moving, release) = self.step_departures(room);
        if let (Some(speed), Some(length)) = (release, self.next_length)
            && let Some(last) = self.departures.last_mut()
        {
            last.is_leaving = true;
            let way = last.pane.swing.way;
            let start = Edge { at: -way * (room + length) / 2.0, speed };
            let swing = Swing { near: start, far: start, way, has_landed: false, has_swung: false };
            self.lead = Some(Pane { swing, length });
            self.next_length = None;
            is_moving = true;
        }
        is_moving |= self.step_lead();
        self.push_forward();
        self.departures.retain(|departure| {
            let swing = &departure.pane.swing;
            !departure.is_leaving || swing.rear().at * swing.way < (room + departure.pane.length) / 2.0
        });
        is_moving
    }

    fn panes(&self) -> impl Iterator<Item = (Option<(&Picture, u64)>, f32, f32)> {
        let departing = self.departures.iter().map(|departure| {
            let swing = &departure.pane.swing;
            (Some((&departure.picture, departure.id)), swing.near.at, swing.far.at)
        });
        departing.chain(self.lead.iter().map(|pane| (None, pane.swing.near.at, pane.swing.far.at)))
    }

    pub(in crate::ui) fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        (room, quarters): (Rect, u8),
        lead: Option<(&Picture, u64)>,
        mut draw_over: impl FnMut(&mut Pen<'_, '_>, Rect, bool),
    ) {
        for (picture, near, far) in self.panes() {
            let is_lead = picture.is_none();
            let Some((picture, id)) = picture.or(lead) else {
                continue;
            };
            let (width, height) = (picture.width as i32, picture.height() as i32);
            let Rect { x: left, y: top, .. } = room.centre(width, height);
            let (near, far) = (round(near), round(far));
            let area = match quarters % 4 {
                0 => Rect::new(left, top + near, width, height + far - near),
                1 => Rect::new(left - far, top, width + far - near, height),
                2 => Rect::new(left, top - far, width, height + far - near),
                _ => Rect::new(left + near, top, width + far - near, height),
            };
            if near == far {
                pen.blit(room, area.x, area.y, picture.width, &picture.words, id);
            } else if area.w > 0 && area.h > 0 {
                pen.stretch(area, room, picture.width, &picture.words, id);
            }
            if let Some(visible) = area.clip(room) {
                draw_over(pen, visible, is_lead);
            }
        }
    }
}
