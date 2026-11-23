use super::View;
use super::action::Action;
use super::bar::BarWords;
use super::power::{WAYS_DOWN, power_item};
use crate::domain::battery::MEASURING;
use crate::domain::cause::Cause;
use crate::domain::device::Part;
use crate::domain::rundown;
use crate::domain::scene::DEBUG;
use crate::domain::settings::BRIGHTEST;
use crate::gfx::frame_stats::{Pacing, Summary, to_millis};
use crate::hw::power::Power;
use crate::input::to_names_text;
use crate::logs::Logging;
use crate::logs::writer::Status;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::screen::Screen;
use crate::ui::view::lines::List;
use crate::ui::words::{JOINING, OFF, cause_word};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const ON: &str = "ON";
pub(crate) const RUNNING: &str = "RUNNING";
const UNMEASURED: &str = "NOT MEASURED";
pub(crate) const PACING_NAME: &str = "PACING TEST";
const ABSENT: &str = "ABSENT";

const fn rundown_word(status: rundown::Status) -> &'static str {
    match status {
        rundown::Status::Off => OFF,
        rundown::Status::Joining => JOINING,
        rundown::Status::Answered => "PINGING",
        rundown::Status::Unanswered => cause_word(Cause::NoAnswer),
        rundown::Status::Offline => cause_word(Cause::NoNetwork),
    }
}

pub(crate) struct Debugging {
    pub(crate) grid: Grid,
    facts: Vec<(Part, Option<String>)>,
    pub(crate) pacing: Pacing,
    pub(super) rundown: rundown::Status,
}

impl Debugging {
    pub(crate) fn new(facts: Vec<(Part, Option<String>)>) -> Debugging {
        Debugging { grid: Grid::empty(), facts, pacing: Pacing::default(), rundown: rundown::Status::Off }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Rundown,
    Logging,
    Pacing,
    Frames,
    Drawing,
    Presenting,
    Version,
    Fact(usize),
    Bootloader,
}

const HEAD: [Line; 7] =
    [Line::Rundown, Line::Logging, Line::Pacing, Line::Frames, Line::Drawing, Line::Presenting, Line::Version];
pub(crate) const PACING_LINE: usize = {
    let mut at = 0;
    while !matches!(HEAD[at], Line::Pacing) {
        at += 1;
    }
    at
};

const fn logging_word(logging: Logging) -> &'static str {
    match logging {
        Logging::Off => OFF,
        Logging::On => ON,
        Logging::Debug => DEBUG,
    }
}

fn is_pacing_line(index: usize) -> bool {
    matches!(HEAD.get(index), Some(Line::Pacing | Line::Frames | Line::Drawing | Line::Presenting))
}

fn write_speech(_: &Ui, text: &mut String) {
    text.push_str("What I measure of myself.");
}

impl Ui {
    pub(crate) fn does_debug_show(&self) -> bool {
        self.screen == Some(Screen::Debug)
    }

    fn debug_count(&self) -> usize {
        HEAD.len() + self.debug.facts.len() + 1
    }

    fn debug_line(&self, index: usize) -> Option<Line> {
        let facts = self.debug.facts.len();
        match index.checked_sub(HEAD.len()) {
            None => HEAD.get(index).copied(),
            Some(at) if at < facts => Some(Line::Fact(at)),
            Some(at) => (at == facts).then_some(Line::Bootloader),
        }
    }

    pub(crate) fn debug_item(&self, index: usize) -> Option<Item> {
        let pacing = &self.debug.pacing;
        let to_value = |summary: Option<Summary>| match (pacing.is_on, summary) {
            (true, _) => MEASURING.into(),
            (false, Some(summary)) => {
                let (low, high) = (to_millis(summary.min_ns), to_millis(summary.max_ns));
                format!("{low} TO {high} MS, MEAN {}", to_millis(summary.mean_ns))
            }
            (false, None) => UNMEASURED.into(),
        };
        Some(match self.debug_line(index)? {
            Line::Rundown => Item::new("RUNDOWN", rundown_word(self.debug.rundown)),
            Line::Logging => Item::new("LOG", logging_word(self.settings.logging)),
            Line::Pacing => Item::new(
                PACING_NAME,
                match (pacing.is_on, pacing.draw.summary()) {
                    (true, _) => RUNNING.into(),
                    (false, Some(summary)) => format!("{OFF}, {} FRAMES", summary.count),
                    (false, None) => String::from(OFF),
                },
            ),
            Line::Frames => Item::figure("FRAME TO FRAME", to_value(pacing.frames.summary())),
            Line::Drawing => Item::figure("DRAWING", to_value(pacing.draw.summary())),
            Line::Presenting => Item::figure("PRESENTING", to_value(pacing.present.summary())),
            Line::Version => Item::figure("FIRMWARE", crate::VERSION),
            Line::Fact(at) => {
                let (fact, value) = &self.debug.facts[at];
                Item::figure(fact.to_name(), value.as_deref().unwrap_or(ABSENT).to_uppercase())
            }
            Line::Bootloader => power_item(Power::Bootloader).with_caption(WAYS_DOWN),
        })
    }

    pub(crate) fn deal_debug(&mut self) {
        self.deal_lines(List::Debug, |_| true);
    }

    pub(super) fn open_debug(&mut self) {
        self.screen = Some(Screen::Debug);
        self.show_lines(List::Debug, self.debug_count(), None);
    }

    pub(super) fn end_debug(&mut self) {
        self.debug.pacing.is_on = false;
    }

    pub(super) fn pick_debug(&mut self) -> Option<Action> {
        let selection = self.debug.grid.selection;
        match self.debug_line(selection)? {
            Line::Rundown => {
                let is_off = self.debug.rundown == rundown::Status::Off;
                self.debug.rundown = if is_off { rundown::Status::Joining } else { rundown::Status::Off };
            }
            Line::Logging => self.settings.logging = self.settings.logging.next(),
            Line::Pacing => {
                self.set_pacing(!self.debug.pacing.is_on);
                return None;
            }
            Line::Bootloader => return Some(Action::Power(Power::Bootloader)),
            Line::Frames | Line::Drawing | Line::Presenting | Line::Version | Line::Fact(_) => {
                self.debug.grid.swing(1.0);
                return None;
            }
        }
        self.deal_lines(List::Debug, |index| index == selection);
        None
    }

    pub(super) fn set_fact(&mut self, fact: Part, value: Option<String>) -> bool {
        let Some(at) = self.debug.facts.iter().position(|entry| entry.0 == fact) else {
            return false;
        };
        if self.debug.facts[at].1 == value {
            return false;
        }
        self.debug.facts[at].1 = value;
        let does_show = self.does_debug_show();
        if does_show {
            self.deal_lines(List::Debug, |index| index == HEAD.len() + at);
        }
        does_show
    }

    /// Takes the names of the open input devices; true if the screen changed.
    #[must_use]
    pub fn set_inputs(&mut self, names: &[&str]) -> bool {
        self.set_fact(Part::Input, Some(to_names_text(names)))
    }

    /// Takes the logs' status; true if the screen changed.
    #[must_use]
    pub fn set_logs(&mut self, status: &Status) -> bool {
        self.set_fact(Part::Logs, Some(status.to_word()))
    }

    /// Starts the frame pacing test, or stops it and keeps its measures.
    pub fn set_pacing(&mut self, is_on: bool) {
        if is_on && !self.does_debug_show() {
            self.open_debug();
        }
        if is_on && !self.debug.pacing.is_on {
            self.debug.pacing = Pacing { is_on: true, ..Pacing::default() };
        }
        self.debug.pacing.is_on = is_on;
        if self.does_debug_show() {
            self.deal_lines(List::Debug, is_pacing_line);
        }
    }

    /// True while the frame pacing test runs.
    pub const fn is_pacing(&self) -> bool {
        self.debug.pacing.is_on
    }

    /// The frame pacing test.
    pub const fn pacing(&self) -> &Pacing {
        &self.debug.pacing
    }

    /// Notes when a frame started drawing, finished drawing and reached the panel.
    pub fn note_frame_presented(&mut self, started_ns: u64, drawn_ns: u64, presented_ns: u64) {
        if self.debug.pacing.is_on {
            self.debug.pacing.frames.record(presented_ns);
            self.debug.pacing.draw.record(drawn_ns.saturating_sub(started_ns));
            self.debug.pacing.present.record(presented_ns.saturating_sub(drawn_ns));
        }
    }

    /// True while the battery rundown is on.
    pub fn is_rundown_on(&self) -> bool {
        self.debug.rundown != rundown::Status::Off
    }

    /// The backlight's brightness.
    pub fn backlight(&self) -> u8 {
        if self.is_rundown_on() { BRIGHTEST } else { self.settings.brightness }
    }

    /// Takes the rundown's status; true if the screen changed.
    #[must_use]
    pub fn set_rundown(&mut self, status: rundown::Status) -> bool {
        let off = rundown::Status::Off;
        if self.debug.rundown == off || status == off || self.debug.rundown == status {
            return false;
        }
        self.debug.rundown = status;
        let does_show = self.does_debug_show();
        if does_show {
            self.deal_lines(List::Debug, |index| HEAD.get(index) == Some(&Line::Rundown));
        }
        does_show
    }

    pub(super) fn view_debug(&self) -> View<'_> {
        let words = BarWords { place: self.place_in(List::Debug), speech: Some(write_speech), ..BarWords::default() };
        View::Debug { list: List::Debug, name: DEBUG, words }
    }
}
