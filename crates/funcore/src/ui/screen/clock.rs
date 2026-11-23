use super::View;
use super::action::Action;
use super::bar::BarWords;
use crate::base::time::offset::{self, OFFSETS, to_local};
use crate::base::time::setting::{Part, step_local_part};
use crate::base::time::{Moment, SECONDS_PER_MINUTE};
use crate::domain::clock::TimeAnswer;
use crate::domain::scene::CLOCK;
use crate::gfx::icon::Icon;
use crate::gfx::motion::{Edge, NUDGE_SPEED};
use crate::ui::Ui;
use crate::ui::gauge::{Glow, Scale};
use crate::ui::item::Item;
use crate::ui::kit::read_icon;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::words::{CLOCK_RIGHT, CLOCK_SET, NO_TIME, cause_word};
use alloc::format;
use alloc::string::String;
use core::fmt::{Display, Write};

const OFFSET: &str = "OFFSET";
const ASKING: &str = "ASKING";
const PARTS: [Option<Part>; 6] =
    [None, Some(Part::Hour), Some(Part::Minute), Some(Part::Day), Some(Part::Month), Some(Part::Year)];

const fn part_name(part: Part) -> &'static str {
    match part {
        Part::Hour => "HOUR",
        Part::Minute => "MINUTE",
        Part::Day => "DAY",
        Part::Month => "MONTH",
        Part::Year => "YEAR",
    }
}

pub(crate) struct Face {
    icon: Icon,
    pub(crate) mark: Edge,
    pub(super) is_asking: bool,
    setting: Option<Part>,
    date: String,
    // Where the day, the month and the year stand in the date, each from one byte to another.
    date_spans: [(usize, usize); 3],
}

impl Face {
    pub(crate) fn new() -> Face {
        Face {
            icon: read_icon(icon!("schedule")),
            mark: Edge::still(0.0),
            is_asking: false,
            setting: None,
            date: String::new(),
            date_spans: [(0, 0); 3],
        }
    }
}

fn write_speech(ui: &Ui, text: &mut String) {
    match ui.face.setting {
        None => {
            text.push_str("My clock is at ");
            offset::write_text(text, ui.settings.offset);
            text.push('.');
        }
        Some(Part::Hour) => text.push_str("Setting the hour."),
        Some(Part::Minute) => text.push_str("Setting the minute."),
        Some(Part::Day) => text.push_str("Setting the day."),
        Some(Part::Month) => text.push_str("Setting the month."),
        Some(Part::Year) => text.push_str("Setting the year."),
    }
}

impl Ui {
    pub(super) fn open_clock(&mut self) -> Option<Action> {
        (self.note, self.face.mark, self.face.setting) = (None, Edge::still(self.mark_aim()), None);
        self.screen = Some(Screen::Clock);
        self.show_settings();
        (!core::mem::replace(&mut self.face.is_asking, true)).then_some(Action::Time)
    }

    // The moment the clock on screen shows, none while the time is not known.
    pub(super) fn local_now(&self) -> Option<Moment> {
        self.utc.map(|utc| Moment::from_seconds(to_local(utc, self.settings.offset)))
    }

    pub(crate) fn update_clock(&mut self) -> bool {
        let now = self.local_now();
        let clock = now.map(|now| (now.to_time_text(), format!("{} {}", now.weekday_name(), now.day)));
        let has_changed = self.clock != clock;
        self.clock = clock;
        self.face.date.clear();
        self.face.date_spans = [(0, 0); 3];
        if let Some(now) = now {
            let date = &mut self.face.date;
            date.push_str(now.weekday_name());
            let parts: [&dyn Display; 3] = [&now.day, &now.month_name(), &now.year];
            self.face.date_spans = parts.map(|part| {
                date.push(' ');
                let start = date.len();
                let _ = write!(date, "{part}");
                (start, date.len())
            });
        }
        self.deal_settings();
        has_changed
    }

    pub(super) fn today(&self) -> Option<(u32, u8, u8)> {
        self.local_now().map(|now| (now.year, now.month, now.day))
    }

    fn set_part(&mut self, part: Part, steps: i32) -> Option<Action> {
        self.utc = Some(step_local_part(self.utc?, self.settings.offset, part, steps));
        self.update_clock();
        Some(Action::Clock(part, steps))
    }

    fn part_place(&self) -> usize {
        PARTS.iter().position(|part| *part == self.face.setting).unwrap_or(0)
    }

    pub(super) fn clock_parts(&self) -> (usize, usize) {
        (PARTS.len(), self.part_place())
    }

    pub(super) fn choose_next_part(&mut self) {
        self.face.setting = PARTS[(self.part_place() + 1) % PARTS.len()];
        self.deal_settings();
    }

    /// Takes an offset from UTC set elsewhere; true if the screen changed.
    #[must_use]
    pub fn set_offset(&mut self, offset: i16) -> bool {
        self.set_settings(crate::domain::settings::Settings { offset, ..self.settings })
    }

    fn step_offset(&mut self, steps: i32) {
        let previous = self.settings.offset;
        self.settings = self.settings.step_offset(steps);
        if self.settings.offset == previous && steps != 0 {
            self.face.mark.speed += steps.signum() as f32 * NUDGE_SPEED;
        }
        self.update_clock();
    }

    pub(super) fn wheel_clock(&mut self, steps: i32) -> Option<Action> {
        match (self.face.setting, steps) {
            (_, 0) => None,
            (None, steps) => {
                self.step_offset(steps);
                None
            }
            (Some(part), steps) => self.set_part(part, steps),
        }
    }

    pub(super) fn part_item(&self, index: usize) -> Option<Item> {
        let part = *PARTS.get(index)?;
        let time = self.clock.as_ref().map_or_else(String::new, |clock| format!("{} {}", clock.0, self.face.date));
        let Some(part) = part else {
            return Some(Item::primary(OFFSET, offset::to_text(self.settings.offset), None, time));
        };
        let value = self.local_now().map_or_else(
            || NO_TIME.into(),
            |now| match part {
                Part::Hour => format!("{:02}", now.hour),
                Part::Minute => format!("{:02}", now.minute),
                Part::Day => format!("{}", now.day),
                Part::Month => now.month_name().into(),
                Part::Year => format!("{}", now.year),
            },
        );
        Some(Item::primary(part_name(part), value, None, time))
    }

    /// Takes the network's answer on the time; true if the screen changed.
    #[must_use]
    pub fn take_time_answer(&mut self, answer: TimeAnswer, now_ns: u64) -> bool {
        let (word, is_success) = match answer {
            TimeAnswer::Answered { is_moved: true } => (CLOCK_SET, true),
            TimeAnswer::Answered { is_moved: false } => (CLOCK_RIGHT, true),
            TimeAnswer::NoTime => (NO_TIME, false),
            TimeAnswer::Failed(cause) => (cause_word(cause), false),
        };
        self.face.is_asking = false;
        self.react(if is_success { Reaction::Nod } else { Reaction::Slump }, now_ns);
        self.set_note(word.into(), false, now_ns)
    }

    /// Sets the clock from the seconds of UTC; true if the picture changed.
    #[must_use]
    pub fn set_time(&mut self, seconds: u64) -> bool {
        if !self.is_awake() {
            return false;
        }
        let has_bell_change = self.update_bell(seconds);
        let minute = |seconds: u64| seconds / SECONDS_PER_MINUTE;
        if self.utc.replace(seconds).map(minute) == Some(minute(seconds)) {
            return has_bell_change;
        }
        let does_show = self.does_time_show() || self.screen == Some(Screen::Clock);
        self.update_clock() && does_show || has_bell_change
    }

    pub(super) fn view_clock<'a>(&'a self, word: &'a mut String) -> View<'a> {
        let is_status = if let Some((note, _)) = &self.note {
            word.push_str(note);
            true
        } else if self.face.is_asking {
            word.push_str(ASKING);
            true
        } else {
            match self.face.setting {
                None => offset::write_text(word, self.settings.offset),
                Some(part) => word.push_str(part_name(part)),
            }
            false
        };
        let word: &'a str = word;
        let words = BarWords {
            status: is_status.then_some(word),
            subject: (!is_status).then_some(word),
            speech: Some(write_speech),
            ..BarWords::default()
        };
        let [day, month, year] = self.face.date_spans;
        let glow = self.face.setting.map(|part| match part {
            Part::Hour => Glow::Hour,
            Part::Minute => Glow::Minute,
            Part::Day => Glow::Date(day),
            Part::Month => Glow::Date(month),
            Part::Year => Glow::Date(year),
        });
        let scale = Scale {
            icon: &self.face.icon,
            ticks: &OFFSETS,
            mark: self.face.mark.at,
            glow,
            is_set: !self.does_rail_lead(),
            time: self.clock.as_ref().map(|clock| clock.0.as_str()),
            date: &self.face.date,
        };
        View::Clock { name: CLOCK, words, scale }
    }
}
