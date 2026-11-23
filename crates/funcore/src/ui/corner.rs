use super::field::draw_field;
use super::kit::{IconPlace, Kit, find_glyph, read_icon, trace};
use super::ruler::Ruler;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::gfx::Rect;
use crate::gfx::icon::{Icon, Point};
use crate::gfx::mask::Mask;
use crate::gfx::pen::Pen;
use crate::hw::power::battery::Supply;
use crate::wifi::radio::Way;
use alloc::vec::Vec;

pub(super) const WIFI_ICON: &str = icon!("wifi");
pub(super) const CELL_ICON: &str = icon!("signal_cellular_alt");
const BELL_SET: &str = icon!("notifications");
const BELL_RINGING: &str = icon!("notifications_active");
pub(super) const BELL_ICONS: [&str; 2] = [BELL_SET, BELL_RINGING];
const FULL_PERCENT: u8 = 100;
const DRAINING: [&str; 8] = [
    icon!("battery_0_bar"),
    icon!("battery_1_bar"),
    icon!("battery_2_bar"),
    icon!("battery_3_bar"),
    icon!("battery_4_bar"),
    icon!("battery_5_bar"),
    icon!("battery_6_bar"),
    icon!("battery_full"),
];
const UNKNOWN: &str = icon!("battery_unknown");
const UNKNOWN_PLUGGED: &str = icon!("battery_status_good");
const CHARGING: [(u8, &str); 7] = [
    (20, icon!("battery_charging_20")),
    (30, icon!("battery_charging_30")),
    (50, icon!("battery_charging_50")),
    (60, icon!("battery_charging_60")),
    (80, icon!("battery_charging_80")),
    (90, icon!("battery_charging_90")),
    (FULL_PERCENT, icon!("battery_charging_full")),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CornerStyle {
    Foot,
    Top,
    Rail,
    Hidden,
}

impl CornerStyle {
    pub(super) fn area(self, ruler: &Ruler) -> Option<Rect> {
        match self {
            CornerStyle::Foot => {
                let side = round(ruler.band) + 2 * ruler.gap;
                Some(Rect::new(ruler.width - side, ruler.height - side, side, side))
            }
            CornerStyle::Top => {
                let side = ruler.top_line_height();
                Some(Rect::new(ruler.width - side, 0, side, side))
            }
            CornerStyle::Rail => Some(Rect::new(0, ruler.height - ruler.rail_foot(), ruler.left, ruler.rail_foot())),
            CornerStyle::Hidden => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Bell {
    Set,
    Ringing,
}

pub(super) struct CornerFacts<'a> {
    pub(super) clock: Option<(&'a str, &'a str)>,
    pub(super) link: Option<Way>,
    pub(super) bell: Option<Bell>,
    pub(super) bell_time: Option<&'a str>,
    pub(super) is_low: bool,
    pub(super) is_on_cable: bool,
}

pub(super) struct Corner {
    icons: Vec<Icon>,
    extent: (Point, Point),
    // The battery's body, which every icon draws and the charging ones mark beside.
    body: (Point, Point),
    icon: Option<usize>,
}

fn battery_icon(level: Option<u8>, supply: Supply) -> usize {
    let (bars, full) = (DRAINING.len() - 1, usize::from(FULL_PERCENT));
    match (level.map(usize::from), supply) {
        (Some(level), Supply::Battery) => (0..=bars).min_by_key(|bar| (level * bars).abs_diff(bar * full)).unwrap_or(0),
        (Some(level), Supply::Cable | Supply::Charging | Supply::Charged) => {
            let nearest = (0..CHARGING.len()).min_by_key(|at| level.abs_diff(usize::from(CHARGING[*at].0)));
            DRAINING.len() + nearest.unwrap_or(0)
        }
        (None, Supply::Battery) => DRAINING.len() + CHARGING.len(),
        (None, _) => DRAINING.len() + CHARGING.len() + 1,
    }
}

impl Corner {
    pub(super) fn new() -> Corner {
        let files =
            DRAINING.iter().copied().chain(CHARGING.iter().map(|entry| entry.1)).chain([UNKNOWN, UNKNOWN_PLUGGED]);
        let icons: Vec<Icon> = files.map(read_icon).collect();
        let union = |(low, high): (Point, Point), (a, b): (Point, Point)| {
            (Point { x: low.x.min(a.x), y: low.y.min(a.y) }, Point { x: high.x.max(b.x), y: high.y.max(b.y) })
        };
        let bounds = |icons: &[Icon]| {
            icons
                .iter()
                .filter_map(Icon::extent)
                .reduce(union)
                .unwrap_or((Point { x: 0.0, y: 0.0 }, Point { x: 1.0, y: 1.0 }))
        };
        let (extent, body) = (bounds(&icons), bounds(&icons[..DRAINING.len()]));
        Corner { icons, extent, body, icon: None }
    }

    // Takes the level and the supply the battery's icon shows; true if it is another icon.
    pub(super) fn show(&mut self, level: Option<u8>, supply: Supply) -> bool {
        let icon = Some(battery_icon(level, supply));
        core::mem::replace(&mut self.icon, icon) != icon
    }

    fn trace_battery(&self, mask: &mut Mask, right: f32, top: f32, side: f32) {
        let Some(icon) = self.icon.and_then(|index| self.icons.get(index)) else {
            return;
        };
        let (low, high) = self.extent;
        let scale = side / (high.y - low.y);
        trace(mask, icon, right - high.x * scale, top - low.y * scale, scale);
    }

    fn draw_battery(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, facts: &CornerFacts<'_>, at: (i32, f32), side: f32) {
        let ((right, top), ground) = (at, kit.roles.background);
        pen.cover(battery_colour(kit, facts), ground, |mask| self.trace_battery(mask, right as f32, top, side));
        let (low, high) = self.extent;
        let mut left = right - round(side * (high.x - low.x) / (high.y - low.y));
        if let Some(mark) = link_mark(kit, facts) {
            left -= kit.ruler.gap + round(side);
            kit.draw_icon(pen, mark, IconPlace { left: left as f32, top, side }, (kit.roles.text, ground));
        }
        draw_bell(pen, kit, facts, (left, top), side);
    }

    pub(super) fn draw(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, style: CornerStyle, facts: &CornerFacts<'_>) {
        let (ruler, roles, gap) = (kit.ruler, kit.roles, kit.ruler.gap);
        match style {
            CornerStyle::Foot => {
                let top = (ruler.height - gap) as f32 - ruler.band;
                self.draw_battery(pen, kit, facts, (ruler.width - gap, top), ruler.band);
                if let Some((time, day)) = facts.clock {
                    let left = ruler.under_bar().x;
                    pen.text(None, &ruler.heading, (left, ruler.foot_top()), time, roles.text, 1);
                    pen.text(None, &ruler.body, (left, ruler.height - gap - ruler.small()), day, roles.text_dim, 1);
                }
            }
            CornerStyle::Top => {
                pen.fill(Rect::new(0, 0, ruler.width, ruler.top_line_height()), roles.background);
                if let Some((time, day)) = facts.clock {
                    let day_left = pen.text(None, &ruler.heading, (gap, gap), time, roles.text, 1) + gap;
                    let day_top = gap + ruler.large() - ruler.small();
                    pen.text(None, &ruler.body, (day_left, day_top), day, roles.text_dim, 1);
                }
                self.draw_battery(pen, kit, facts, (ruler.width - gap, gap as f32), ruler.large() as f32);
            }
            CornerStyle::Rail => self.draw_rail_foot(pen, kit, facts),
            CornerStyle::Hidden => {}
        }
    }

    // Under the rail's icons: the battery beside the network's mark over the bell, centred on the rail as one, and the time or the timer's under them.
    fn draw_rail_foot(&self, pen: &mut Pen<'_, '_>, kit: &Kit<'_>, facts: &CornerFacts<'_>) {
        let (ruler, roles, gap) = (kit.ruler, kit.roles, kit.ruler.gap);
        let (side, small) = (ruler.rail_side(), ruler.small());
        let centre = gap + side / 2;
        let time_top = ruler.height - gap - small;
        let (height, top) = (2 * small, time_top - gap - 2 * small);
        if let Some(time) = facts.bell_time.or(facts.clock.map(|clock| clock.0)) {
            let width = ruler.body.text_width(time).min(side);
            draw_field(kit, pen, &ruler.body, (centre - width / 2, time_top), time, width, roles.text);
        }
        let ((low, high), (body_low, body_high)) = (self.extent, self.body);
        let scale = height as f32 / (high.y - low.y);
        let body = round((body_high.x - body_low.x) * scale);
        let left = centre - (body + gap + small) / 2;
        let right = left as f32 + (high.x - body_low.x) * scale;
        pen.cover(battery_colour(kit, facts), roles.background, |mask| {
            self.trace_battery(mask, right, top as f32, height as f32);
        });
        let link = link_mark(kit, facts).map(|icon| (icon, roles.text));
        let (column, mark) = ((left + body + gap) as f32, small as f32);
        // The first mark's drawing begins at the battery's top, the second's ends at its foot.
        for (at, (icon, colour)) in [link, bell_mark(kit, facts)].into_iter().flatten().enumerate() {
            let Some((icon_low, icon_high)) = icon.extent() else {
                continue;
            };
            let box_top =
                if at == 0 { top as f32 - icon_low.y * mark } else { (top + height) as f32 - icon_high.y * mark };
            kit.draw_icon(pen, icon, IconPlace { left: column, top: box_top, side: mark }, (colour, roles.background));
        }
    }
}

const fn battery_colour(kit: &Kit<'_>, facts: &CornerFacts<'_>) -> Color {
    if facts.is_on_cable {
        kit.roles.saved
    } else if facts.is_low {
        kit.roles.alert
    } else {
        kit.roles.text
    }
}

fn link_mark<'a>(kit: &Kit<'a>, facts: &CornerFacts<'_>) -> Option<&'a Icon> {
    let icon = match facts.link? {
        Way::Wifi => WIFI_ICON,
        Way::Cell => CELL_ICON,
    };
    find_glyph(kit.glyphs, icon)
}

fn bell_mark<'a>(kit: &Kit<'a>, facts: &CornerFacts<'_>) -> Option<(&'a Icon, Color)> {
    let (icon, colour) = match facts.bell? {
        Bell::Ringing => (BELL_RINGING, kit.roles.alert),
        Bell::Set => (BELL_SET, kit.roles.text),
    };
    find_glyph(kit.glyphs, icon).map(|glyph| (glyph, colour))
}

fn draw_bell(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, facts: &CornerFacts<'_>, (right, top): (i32, f32), side: f32) {
    let (ruler, roles, gap, mut right) = (kit.ruler, kit.roles, kit.ruler.gap, right);
    if let Some((glyph, colour)) = bell_mark(kit, facts) {
        right -= gap + round(side);
        kit.draw_icon(pen, glyph, IconPlace { left: right as f32, top, side }, (colour, roles.background));
    }
    if let Some(text) = facts.bell_time {
        let x = right - gap - ruler.heading.text_width(text);
        let y = round(top + (side - ruler.large() as f32) / 2.0);
        pen.text(None, &ruler.heading, (x, y), text, roles.text, 1);
    }
}
