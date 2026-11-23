use super::action::Action;
use super::bar::BarWords;
use super::voice::{RequestLine, RequestList, VoiceId};
use super::{BACKWARD, FORWARD, Screen, View, WindowContent};
use crate::base::time::{NANOS_PER_SECOND, write_duration};
use crate::domain::bell::state::{Report, Standing};
use crate::domain::bell::{Bell, Change, Entry, Kind, RING_S};
use crate::domain::chat::tools::Destination;
use crate::domain::scene::BELL;
use crate::domain::uses::Use;
use crate::domain::voice::Bid;
use crate::hw::sound::volume::QUIETEST;
use crate::input::Event;
use crate::ui::Ui;
use crate::ui::corner;
use crate::ui::grid::Grid;
use crate::ui::item::{Aside, Item};
use crate::ui::robot::Reaction;
use crate::ui::view::lines::List;
use crate::ui::view::window::Window;
use crate::ui::words::{RINGING, WHAT_YOU_SAY};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const CANCEL_ICON: &str = icon!("delete");
pub(super) const ICONS: [&str; 1] = [CANCEL_ICON];
const ALARM: &str = "ALARM";
const REMINDER: &str = "REMINDER";
const TIMER: &str = "TIMER";
pub(super) const NONE_SET: &str = "NONE SET";
const RINGS: &str = "RINGS";
const NEW: &str = "NEW";
const HOLD_AND_SAY: &str = "HOLD AND SAY WHEN";
const SET: &str = "SET";
const TEXT: &str = "TEXT";
const CANCEL: &str = "CANCEL";
// The list's first line takes a request; what is set follows it.
const FIRST_ENTRY: usize = 1;

const fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Alarm => ALARM,
        Kind::Reminder => REMINDER,
        Kind::Timer => TIMER,
    }
}

const fn cancel_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Alarm => "THIS ALARM",
        Kind::Reminder => "THIS REMINDER",
        Kind::Timer => "THIS TIMER",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageLine {
    When,
    Text,
    Cancel,
}

fn page_lines(entry: &Entry) -> impl Iterator<Item = PageLine> {
    let text = (!entry.text.is_empty()).then_some(PageLine::Text);
    [Some(PageLine::When), text, Some(PageLine::Cancel)].into_iter().flatten()
}

pub(crate) struct Bells {
    set: Bell,
    is_changed: bool,
    // What rang and shows in the window until it is let go.
    rung: Vec<Entry>,
    start_s: Option<u64>,
    is_silencing: bool,
    seconds: Option<u64>,
    // The seconds as the timer shows them, written where they change.
    time_left: String,
    pub(crate) window: Window,
    pub(crate) grid: Grid,
    pub(crate) list: Grid,
    pub(crate) page: Grid,
    // What is set as the list shows it, the soonest first.
    listed: Vec<Entry>,
    page_entry: Option<Entry>,
    pub(super) request: RequestLine<Standing>,
}

impl Bells {
    pub(crate) fn new() -> Bells {
        Bells {
            set: Bell::default(),
            is_changed: false,
            rung: Vec::new(),
            start_s: None,
            is_silencing: false,
            seconds: None,
            time_left: String::new(),
            window: Window::new(BELL),
            grid: Grid::empty(),
            list: Grid::empty(),
            page: Grid::empty(),
            listed: Vec::new(),
            page_entry: None,
            request: RequestLine::new(),
        }
    }
}

fn write_list_speech(ui: &Ui, text: &mut String) {
    let _ = match ui.bells.listed.len() {
        0 => text.write_str("Nothing is set."),
        1 => text.write_str("One ring is set."),
        count => write!(text, "{count} rings are set."),
    };
}

fn write_ring_speech(_: &Ui, text: &mut String) {
    text.push_str("It is time.");
}

impl Ui {
    /// The bell's next ring, in seconds of UTC.
    pub fn next_bell_s(&self) -> Option<u64> {
        self.bells.set.next_s(self.settings.offset)
    }

    pub(super) fn does_ring(&self) -> bool {
        self.bells.start_s.is_some()
    }

    pub(in crate::ui) fn bell(&self) -> Option<corner::Bell> {
        if self.does_ring() {
            Some(corner::Bell::Ringing)
        } else {
            self.bells.set.has_clock().then_some(corner::Bell::Set)
        }
    }

    /// True while the ring's sound plays: the bell rings, or a call rings here.
    pub fn does_ring_sound(&self) -> bool {
        (self.does_ring() || self.does_call_ring()) && self.settings.sounds > QUIETEST
    }

    /// Takes the bell an earlier run kept; true if the screen changed.
    #[must_use]
    pub fn recall_bell(&mut self, kept: Bell) -> bool {
        let set = core::mem::replace(&mut self.bells.set, kept);
        self.bells.set.merge(set);
        self.note_bell_change()
    }

    /// The bell to keep, where it changed since it was last taken, once what an earlier run kept is read.
    pub fn take_changed_bell(&mut self) -> Option<Bell> {
        if !self.bells.is_changed || !self.are_settled() {
            return None;
        }
        self.bells.is_changed = false;
        Some(self.bells.set.clone())
    }

    /// Rings what is due at `utc`, in a window over what shows.
    pub fn ring(&mut self, utc: u64, now_ns: u64) -> Option<Action> {
        let due = self.bells.set.take_due(utc, self.settings.offset);
        if due.is_empty() {
            return None;
        }
        self.bells.rung.extend(due);
        (self.bells.start_s, self.bells.is_changed, self.is_dark) = (Some(utc), true, false);
        if self.bells.window.is_open {
            self.lay_ring();
        } else {
            self.note_bell_change();
            self.show_ring(now_ns);
        }
        (!self.is_awake()).then_some(Action::Wake)
    }

    // Raises the window of what rang over what shows, once neither the passphrase's, the key's nor a ringing call's window stands.
    pub(in crate::ui) fn show_ring(&mut self, now_ns: u64) {
        let is_barred = self.safe.is_shut
            || self.safe.window.is_open
            || self.keying.window.is_open
            || self.screen == Some(Screen::Incoming);
        if self.bells.rung.is_empty() || self.bells.window.is_open || is_barred {
            return;
        }
        self.leave_screen();
        if self.pressed_on.is_some() {
            self.let_go(now_ns);
        }
        self.passage = None;
        self.bells.window.rise_over(self.screen, self.ruler.height);
        self.screen = Some(Screen::Ring);
        self.lay_ring();
    }

    pub(in crate::ui) fn lay_ring(&mut self) {
        let count = self.bells.rung.len();
        let grid = &mut self.bells.grid;
        grid.begin();
        grid.set_items_at(count, 0);
        grid.land();
        self.deal_lines(List::Ring, |_| true);
    }

    pub(in crate::ui) fn close_ring(&mut self) {
        (self.bells.start_s, self.bells.window.is_closing) = (None, true);
        self.bells.grid.press(false);
    }

    pub(in crate::ui) fn ring_gone(&mut self) {
        self.bells.window.is_closing = false;
        self.bells.rung.clear();
        self.note_bell_change();
        if let Some(list) = self.visible_list() {
            self.deal_lines(list, |_| true);
        }
        self.keep_asking();
    }

    // Lets go of the window and what rang, as home takes its place.
    pub(in crate::ui) fn drop_ring(&mut self) {
        if self.bells.window.is_open {
            (self.bells.window.is_open, self.bells.window.is_closing, self.bells.start_s) = (false, false, None);
            self.bells.rung.clear();
        }
    }

    // Takes the window down as the key's takes the screen: what rang and was not let go shows once that has gone.
    pub(super) fn lower_ring(&mut self) {
        if core::mem::take(&mut self.bells.window.is_closing) {
            self.bells.rung.clear();
        }
        self.bells.window.is_open = false;
    }

    pub(super) fn take_silence(&mut self, event: Event) -> bool {
        match event {
            Event::PttDown if self.does_ring() => {
                (self.bells.start_s, self.bells.is_silencing) = (None, true);
                true
            }
            Event::PttUp if self.bells.is_silencing => {
                self.bells.is_silencing = false;
                true
            }
            _ => false,
        }
    }

    fn does_bell_show(&self) -> bool {
        !self.is_dark && self.does_time_show()
    }

    pub(super) fn update_bell(&mut self, utc: u64) -> bool {
        let has_ended = self.bells.start_s.is_some_and(|start_s| utc >= start_s + RING_S);
        if has_ended {
            self.bells.start_s = None;
        }
        let does_show = self.does_bell_show();
        let seconds = self.bells.set.time_left_s(utc).filter(|_| does_show);
        let has_new_count = core::mem::replace(&mut self.bells.seconds, seconds) != seconds;
        if has_new_count {
            self.bells.time_left.clear();
            if let Some(seconds) = seconds {
                write_duration(&mut self.bells.time_left, seconds);
            }
        }
        has_ended && (does_show || self.screen == Some(Screen::Ring)) || has_new_count && does_show
    }

    // The time left of the timer that runs out first, as it shows, while it counts.
    pub(in crate::ui) fn bell_time(&self) -> Option<&str> {
        self.bells.seconds.map(|_| self.bells.time_left.as_str())
    }

    pub(crate) fn bell_due(&self) -> Option<u64> {
        (self.does_ring() || self.bells.seconds.is_some()).then_some(self.ticking.tick_ns + NANOS_PER_SECOND)
    }

    pub(super) fn write_bell(&self, text: &mut String) {
        let word = if self.does_ring() { Some(RINGING) } else { self.bells.set.has_clock().then_some(ALARM) };
        for part in word.into_iter().chain(self.bell_time()) {
            text.push(' ');
            text.push_str(part);
        }
    }

    pub(super) fn bell_for_model(&self) -> String {
        self.bells.set.to_model_text(self.utc, self.settings.offset)
    }

    // Makes a change the conversation asked: its answer to the model, and whether what shows changed.
    pub(super) fn change_bell(&mut self, change: Change) -> (String, bool) {
        let (told, is_changed) = self.bells.set.apply(change, self.utc, self.settings.offset);
        if is_changed {
            self.bells.is_changed = true;
            self.notice(false);
        }
        (told, is_changed && self.note_bell_change())
    }

    // What shows of the bell follows it: its list, an entry's page, the menu's line and the corner; true where any shows.
    pub(in crate::ui) fn note_bell_change(&mut self) -> bool {
        let is_set = self.bells.page_entry.as_ref().is_some_and(|entry| self.bells.set.holds(entry));
        if self.screen == Some(Screen::BellEntry) && !is_set {
            (self.screen, self.bells.page_entry) = (Some(Screen::Bell), None);
        }
        let is_listed = matches!(self.screen, Some(Screen::Bell | Screen::BellEntry));
        if self.screen == Some(Screen::Bell) {
            self.lay_bell_list();
        }
        let is_line = self.redeal_lines(&[Use::Open(Destination::Bell)]);
        is_listed || is_line || self.does_bell_show()
    }

    pub(super) fn bell_count(&self) -> usize {
        self.bells.set.len()
    }

    pub(super) fn open_bell(&mut self) {
        self.bells.listed = self.bells.set.sorted(self.settings.offset);
        self.show_lines(List::Bell, self.bells.listed.len() + FIRST_ENTRY, None);
        self.screen = Some(Screen::Bell);
    }

    // Lays the list anew as the bell stands, on the line it stood on.
    pub(in crate::ui) fn lay_bell_list(&mut self) {
        self.bells.listed = self.bells.set.sorted(self.settings.offset);
        let selection = self.bells.list.selection;
        self.show_lines(List::Bell, self.bells.listed.len() + FIRST_ENTRY, Some(selection));
    }

    pub(super) fn does_bell_list_show(&self) -> bool {
        self.screen == Some(Screen::Bell)
    }

    /// True while the bell's list shows and the device is awake.
    pub fn is_bell_wanted(&self) -> bool {
        self.is_awake() && self.does_bell_list_show()
    }

    /// The bids the bell's list made since this was last taken.
    pub fn take_bell_bids(&mut self) -> Vec<Bid<Standing>> {
        self.bells.request.take_bids()
    }

    // The bell as it stands, which a request is read against.
    pub(super) fn bell_standing(&self) -> Standing {
        Standing(self.bells.set.clone(), self.settings.offset)
    }

    /// Takes the report of the bell's requests, and makes the changes one asked; true if the screen changed.
    #[must_use]
    pub fn take_bell_report(&mut self, report: Report, now_ns: u64) -> bool {
        let has_new_phase = self.take_phase(VoiceId::Bell, report.phase, now_ns);
        let before = self.bells.set.clone();
        let has_asked = !report.changes.is_empty();
        let mut is_changed = false;
        for change in report.changes {
            is_changed |= self.change_bell(change).1;
        }
        if has_asked {
            self.react(if self.bells.set == before { Reaction::Shake } else { Reaction::Nod }, now_ns);
        }
        // The list stands on what the request set.
        let set = self.bells.listed.iter().position(|entry| !before.holds(entry));
        if let Some(at) = set.filter(|_| self.does_bell_list_show()) {
            self.bells.list.choose(at + FIRST_ENTRY);
        }
        if (has_new_phase || has_asked) && self.does_bell_list_show() {
            self.deal_lines(List::Bell, |_| true);
        }
        (has_new_phase || has_asked) && self.does_bell_list_show() || is_changed
    }

    fn entry_words(&self, entry: &Entry) -> (String, String) {
        let name = if entry.text.is_empty() { String::from(kind_word(entry.kind())) } else { entry.text.clone() };
        (name, entry.to_when_text(self.settings.offset))
    }

    pub(crate) fn bell_item(&self, index: usize) -> Option<Item> {
        let Some(at) = index.checked_sub(FIRST_ENTRY) else {
            return Some(if self.bells.request.voice.is_listening() {
                Item::new(SET, WHAT_YOU_SAY).with_recording().with_aside(Aside::Meter)
            } else {
                Item::new(NEW, HOLD_AND_SAY).with_recording()
            });
        };
        let (name, when) = self.entry_words(self.bells.listed.get(at)?);
        Some(Item::lead(name, when))
    }

    pub(crate) fn ring_item(&self, index: usize) -> Option<Item> {
        let (name, when) = self.entry_words(self.bells.rung.get(index)?);
        Some(Item::new(name, when))
    }

    pub(crate) fn bell_entry_item(&self, index: usize) -> Option<Item> {
        let entry = self.bells.page_entry.as_ref()?;
        Some(match page_lines(entry).nth(index)? {
            PageLine::When => Item::new(RINGS, entry.to_when_text(self.settings.offset)),
            PageLine::Text => Item::new(TEXT, entry.text.clone()),
            PageLine::Cancel => Item::action(CANCEL, cancel_word(entry.kind()), CANCEL_ICON),
        })
    }

    // A click on the request's line records until the next, as a hold does while it lasts; one on a ring opens its page.
    pub(super) fn pick_bell(&mut self) {
        if self.release_request(RequestList::Bell) {
            return;
        }
        let Some(at) = self.bells.list.selection.checked_sub(FIRST_ENTRY) else {
            return self.press_request(RequestList::Bell);
        };
        let Some(entry) = self.bells.listed.get(at).cloned() else {
            return self.swing_line();
        };
        self.depart(FORWARD);
        let count = page_lines(&entry).count();
        self.bells.page_entry = Some(entry);
        self.show_lines(List::BellEntry, count, None);
        self.screen = Some(Screen::BellEntry);
    }

    pub(super) fn act_on_bell_entry(&mut self, now_ns: u64) {
        let Some(entry) = self.bells.page_entry.clone() else {
            return;
        };
        if page_lines(&entry).nth(self.bells.page.selection) != Some(PageLine::Cancel) {
            return self.bells.page.swing(1.0);
        }
        self.depart(BACKWARD);
        self.bells.is_changed |= self.bells.set.remove(&entry);
        (self.screen, self.bells.page_entry) = (Some(Screen::Bell), None);
        self.lay_bell_list();
        self.react(Reaction::Shake, now_ns);
    }

    pub(super) fn view_bell<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        let has_status = self.bells.request.voice.write_word(now_ns, word);
        let status: &'a str = word;
        let (count, selection) = (self.bells.listed.len(), self.bells.list.selection.checked_sub(FIRST_ENTRY));
        let words = BarWords {
            status: has_status.then_some(status),
            place: selection.filter(|_| count > 0).map(|at| (at + 1, count)),
            subject: (count == 0).then_some(NONE_SET),
            speech: Some(write_list_speech),
            ..BarWords::default()
        };
        View::Lines { list: List::Bell, name: BELL, words }
    }

    pub(super) fn view_bell_entry(&self) -> View<'_> {
        let entry = self.bells.page_entry.as_ref();
        let name = entry.map_or(BELL, |entry| kind_word(entry.kind()));
        let text = entry.map(|entry| entry.text.as_str()).filter(|text| !text.is_empty());
        let place = if text.is_none() { self.place_in(List::BellEntry) } else { None };
        let words = BarWords { place, subject: text, ..BarWords::default() };
        View::Lines { list: List::BellEntry, name, words }
    }

    pub(super) fn view_ring(&self) -> View<'_> {
        let words = BarWords {
            status: self.does_ring().then_some(RINGING),
            place: self.place_in(List::Ring),
            speech: Some(write_ring_speech),
            ..BarWords::default()
        };
        View::Window { words, content: WindowContent::Lines(List::Ring) }
    }
}
