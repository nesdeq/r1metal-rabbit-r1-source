use super::bar::BarWords;
use super::voice::{RequestLine, RequestList, VoiceId};
use super::{BACKWARD, FORWARD, Screen, View};
use crate::domain::chat::tools::Destination;
use crate::domain::news::feed::Feed;
use crate::domain::news::request::{Change, Report, Standing};
use crate::domain::news::silent::Silent;
use crate::domain::news::topics::{Topic, Topics};
use crate::domain::scene::RUNDOWNS;
use crate::domain::uses::Use;
use crate::domain::voice::Bid;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::{Aside, Item};
use crate::ui::robot::Reaction;
use crate::ui::view::lines::List;
use crate::ui::words::{OFF, WHAT_YOU_SAY};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const ADD_ICON: &str = icon!("add");
const DROP_ICON: &str = icon!("close");
const DELETE_ICON: &str = icon!("delete");
const FEED_ICON: &str = icon!("rss_feed");
pub(super) const ICONS: [&str; 4] = [ADD_ICON, DROP_ICON, DELETE_ICON, FEED_ICON];
const NEW: &str = "NEW";
const HOLD_AND_SAY: &str = "OR A CHANGE: HOLD AND SAY IT";
const ADD_OR_CHANGE: &str = "ADD OR CHANGE";
const ADD: &str = "ADD";
const AS_NEW: &str = "AS A NEW DEVICE HAS IT";
const MADE: &str = "MADE";
const EVERY_EDITION: &str = "WITH EVERY EDITION";
const ABOUT: &str = "ABOUT";
const SILENT: &str = "GAVE NO STORY";
const CLICK_TO_REMOVE: &str = "CLICK AGAIN TO REMOVE";
const DELETE: &str = "DELETE";
const THE_RUNDOWN: &str = "THE RUNDOWN";
const CLICK_AGAIN_TO_DELETE: &str = "CLICK AGAIN TO DELETE";
const KEEP: &str = "KEEP";
const AS_IT_IS: &str = "IT AS IT IS";
const CLICK_TO_DELETE: &str = "CLICK TO DELETE";
const NONE_KEPT: &str = "NONE KEPT";
// The list's first line takes a request; the rundowns follow it.
const FIRST_ENTRY: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListLine {
    Request,
    // A rundown, by its place among those kept.
    Topic(usize),
    // One of the two a new device has that is kept no longer.
    Add(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageLine {
    // The two lines over a rundown a request asks to delete: one deletes it, one keeps it.
    Deletion,
    Keep,
    Made,
    About,
    // A feed, by its place among the rundown's.
    Feed(usize),
    Delete,
}

pub(crate) struct Rundowns {
    kept: Topics,
    is_changed: bool,
    silent: Silent,
    pub(crate) list: Grid,
    pub(crate) page: Grid,
    // The rundown whose page shows, and its name as the bar writes it.
    open: Option<String>,
    title: String,
    // The page's line a click armed: the next click on it removes what it stands for.
    armed: Option<usize>,
    // The rundowns a request asks to delete and the owner has neither deleted nor kept, the first on the page.
    deleting: VecDeque<String>,
    pub(super) request: RequestLine<Standing>,
}

impl Rundowns {
    pub(crate) fn new() -> Rundowns {
        Rundowns {
            kept: Topics::default(),
            is_changed: false,
            silent: Silent::default(),
            list: Grid::empty(),
            page: Grid::empty(),
            open: None,
            title: String::new(),
            armed: None,
            deleting: VecDeque::new(),
            request: RequestLine::new(),
        }
    }
}

// A count of feeds as a line tells it.
fn write_feeds(text: &mut String, count: usize) {
    let _ = write!(text, "{count} {}", if count == 1 { "FEED" } else { "FEEDS" });
}

fn write_list_speech(ui: &Ui, text: &mut String) {
    let _ = match ui.rundowns.kept.made().count() {
        0 => text.write_str("No rundown is made."),
        1 => text.write_str("One rundown is made."),
        count => write!(text, "{count} rundowns are made."),
    };
}

impl Ui {
    /// The rundowns the owner keeps.
    pub const fn kept_rundowns(&self) -> &Topics {
        &self.rundowns.kept
    }

    /// Takes the rundowns an earlier run kept; true if the screen changed.
    #[must_use]
    pub fn recall_rundowns(&mut self, kept: Topics) -> bool {
        if !self.rundowns.is_changed {
            self.rundowns.kept = kept;
        }
        self.note_rundowns_change()
    }

    /// The rundowns to keep, where they changed since they were last taken, once what an earlier run kept is read.
    pub fn take_changed_rundowns(&mut self) -> Option<Topics> {
        if !self.rundowns.is_changed || !self.are_settled() {
            return None;
        }
        self.rundowns.is_changed = false;
        Some(self.rundowns.kept.clone())
    }

    // Takes the feeds that gave no story when they were last read; true if the screen changed.
    pub(super) fn set_silent_feeds(&mut self, silent: Silent) -> bool {
        let is_new = core::mem::replace(&mut self.rundowns.silent, silent.clone()) != silent;
        is_new && self.note_rundowns_change()
    }

    pub(super) fn does_rundown_list_show(&self) -> bool {
        self.screen == Some(Screen::Rundowns)
    }

    fn do_rundowns_show(&self) -> bool {
        matches!(self.screen, Some(Screen::Rundowns | Screen::Rundown))
    }

    /// True while the rundowns' list or a rundown's page shows and the device is awake.
    pub fn are_rundowns_wanted(&self) -> bool {
        self.is_awake() && self.do_rundowns_show()
    }

    /// The bids the rundowns' list made since this was last taken.
    pub fn take_rundown_bids(&mut self) -> Vec<Bid<Standing>> {
        self.rundowns.request.take_bids()
    }

    // The rundowns as they stand, which a request is read against.
    pub(super) fn rundowns_standing(&self) -> Standing {
        Standing(self.rundowns.kept.clone(), self.rundowns.silent.clone())
    }

    // Opens the list and hands it a request in words, as the conversation asks one.
    pub(super) fn ask_rundowns(&mut self, request: String) {
        if self.does_rundown_list_show() {
            let standing = self.rundowns_standing();
            self.rundowns.request.ask(standing, request);
        }
    }

    // Makes a change; true where it changed what is kept.
    fn change_rundowns(&mut self, change: Change) -> bool {
        let is_changed = match change {
            Change::Put(topic) => self.rundowns.kept.put(topic),
            Change::Delete(name) => self.rundowns.kept.remove(&name),
        };
        self.rundowns.is_changed |= is_changed;
        is_changed
    }

    // Stands the list on the rundown of that name, where it is kept.
    fn choose_rundown(&mut self, name: &str) {
        if let Some(at) = self.rundowns.kept.all().iter().position(|topic| topic.name == name) {
            self.rundowns.list.choose(at + FIRST_ENTRY);
        }
    }

    /// Takes the report of the rundowns' requests: a rundown one came to is kept at once, the list standing on it, and one it asks to delete shows on its page for the click that deletes it. True if the screen changed.
    #[must_use]
    pub fn take_rundowns_report(&mut self, report: Report, now_ns: u64) -> bool {
        let has_new_phase = self.take_phase(VoiceId::Rundowns, report.phase, now_ns);
        // What a request comes to once the rundowns' screens are left is let go with them.
        let changes = if self.do_rundowns_show() { report.changes } else { Vec::new() };
        let (mut is_made, mut is_asked) = (false, false);
        for change in changes {
            match change {
                Change::Delete(name) => {
                    self.rundowns.deleting.push_back(name);
                    is_asked = true;
                }
                Change::Put(topic) => {
                    let name = topic.name.clone();
                    if self.change_rundowns(Change::Put(topic)) {
                        is_made = true;
                        self.choose_rundown(&name);
                    }
                }
            }
        }
        if is_made {
            self.notice(false);
            self.react(Reaction::Nod, now_ns);
        }
        if is_asked {
            self.depart(FORWARD);
            self.screen = Some(Screen::Rundown);
            self.show_deletion();
            self.redeal_lines(&[Use::Open(Destination::Rundowns)]);
            return true;
        }
        if is_made {
            return self.note_rundowns_change();
        }
        let does_show = self.does_rundown_list_show();
        if has_new_phase && does_show {
            self.deal_lines(List::Rundowns, |_| true);
        }
        has_new_phase && does_show
    }

    // What shows of the rundowns is laid anew as they stand: their list, a rundown's page and the menu's line; true where any shows.
    fn note_rundowns_change(&mut self) -> bool {
        if self.screen == Some(Screen::Rundown) && self.page_topic().is_none() {
            (self.screen, self.rundowns.open) = (Some(Screen::Rundowns), None);
        }
        match self.screen {
            Some(Screen::Rundowns) => self.lay_rundown_list(),
            Some(Screen::Rundown) => {
                let count = self.rundown_page_lines().len();
                let selection = self.rundowns.page.selection.min(count.saturating_sub(1));
                self.show_lines(List::Rundown, count, Some(selection));
            }
            _ => {}
        }
        let is_line = self.redeal_lines(&[Use::Open(Destination::Rundowns)]);
        is_line || self.do_rundowns_show()
    }

    pub(super) fn write_rundowns_word(&self, text: &mut String) {
        let (made, count) = (self.rundowns.kept.made().count(), self.rundowns.kept.all().len());
        let _ = if count == 0 { text.write_str(NONE_KEPT) } else { write!(text, "{made} OF {count} MADE") };
    }

    fn rundown_list_count(&self) -> usize {
        FIRST_ENTRY + self.rundowns.kept.all().len() + self.rundowns.kept.lacking().count()
    }

    fn rundown_list_line(&self, index: usize) -> Option<ListLine> {
        let Some(at) = index.checked_sub(FIRST_ENTRY) else {
            return Some(ListLine::Request);
        };
        match at.checked_sub(self.rundowns.kept.all().len()) {
            None => Some(ListLine::Topic(at)),
            Some(lacking) => self.rundowns.kept.lacking().nth(lacking).map(ListLine::Add),
        }
    }

    pub(super) fn open_rundowns(&mut self) {
        (self.rundowns.open, self.rundowns.armed) = (None, None);
        self.rundowns.deleting.clear();
        self.show_lines(List::Rundowns, self.rundown_list_count(), None);
        self.screen = Some(Screen::Rundowns);
    }

    // Lays the list anew as the rundowns stand, on the line it stood on; a deletion asked and neither made nor refused goes as its page is left.
    pub(in crate::ui) fn lay_rundown_list(&mut self) {
        (self.rundowns.open, self.rundowns.armed) = (None, None);
        self.rundowns.deleting.clear();
        let count = self.rundown_list_count();
        let selection = self.rundowns.list.selection.min(count - 1);
        self.show_lines(List::Rundowns, count, Some(selection));
    }

    pub(crate) fn rundowns_item(&self, index: usize) -> Option<Item> {
        Some(match self.rundown_list_line(index)? {
            ListLine::Request if self.rundowns.request.voice.is_listening() => {
                Item::new(ADD_OR_CHANGE, WHAT_YOU_SAY).with_recording().with_aside(Aside::Meter)
            }
            ListLine::Request => Item::new(NEW, HOLD_AND_SAY).with_recording(),
            ListLine::Topic(at) => {
                let topic = self.rundowns.kept.all().get(at)?;
                let mut word = String::from(OFF);
                if topic.is_on {
                    word.clear();
                    write_feeds(&mut word, topic.feed_lines().len());
                    match self.rundowns.silent.count_in(topic) {
                        0 => {}
                        count => drop(write!(word, ", {count} {SILENT}")),
                    }
                }
                Item::lead(topic.name.to_ascii_uppercase(), word).with_icon(topic.drawing())
            }
            ListLine::Add(name) => Item::action(ADD, format!("{}, {AS_NEW}", name.to_ascii_uppercase()), ADD_ICON),
        })
    }

    // A click on the request's line records until the next, as a hold does while it lasts; one on a rundown opens its page, one on a line that adds a rundown a new device has keeps it again.
    pub(super) fn pick_rundown(&mut self, now_ns: u64) {
        if self.release_request(RequestList::Rundowns) {
            return;
        }
        match self.rundown_list_line(self.rundowns.list.selection) {
            Some(ListLine::Request) => self.press_request(RequestList::Rundowns),
            Some(ListLine::Topic(at)) => {
                let Some(name) = self.rundowns.kept.all().get(at).map(|topic| topic.name.clone()) else {
                    return self.swing_line();
                };
                self.depart(FORWARD);
                (self.rundowns.title, self.rundowns.open) = (name.to_ascii_uppercase(), Some(name));
                self.rundowns.armed = None;
                self.screen = Some(Screen::Rundown);
                self.show_lines(List::Rundown, self.rundown_page_lines().len(), None);
            }
            // The rundown takes the line's place among those kept, and the list stands on it.
            Some(ListLine::Add(name)) => {
                self.change_rundowns(Change::Put(Topic::named(name, true)));
                self.choose_rundown(name);
                self.react(Reaction::Nod, now_ns);
                self.note_rundowns_change();
            }
            None => self.swing_line(),
        }
    }

    // The rundown the page shows: the first a request asks to delete, else the one opened.
    fn page_topic(&self) -> Option<&Topic> {
        let name = self.rundowns.deleting.front().or(self.rundowns.open.as_ref())?;
        self.rundowns.kept.get(name)
    }

    fn rundown_page_lines(&self) -> Vec<PageLine> {
        let Some(topic) = self.page_topic() else {
            return Vec::new();
        };
        let is_asked = !self.rundowns.deleting.is_empty();
        let head = is_asked.then_some([PageLine::Deletion, PageLine::Keep]).into_iter().flatten();
        let about = (!topic.about.is_empty()).then_some(PageLine::About);
        let feeds = (0..topic.feed_lines().len()).map(PageLine::Feed);
        let delete = (!is_asked).then_some(PageLine::Delete);
        head.chain([PageLine::Made]).chain(about).chain(feeds).chain(delete).collect()
    }

    // The page of the first rundown a request asks to delete, with its name on the bar.
    fn show_deletion(&mut self) {
        let Some(name) = self.rundowns.deleting.front().map(|name| name.to_ascii_uppercase()) else {
            return;
        };
        (self.rundowns.title, self.rundowns.armed) = (name, None);
        self.show_lines(List::Rundown, self.rundown_page_lines().len(), None);
    }

    fn feed_item(&self, topic: &Topic, at: usize, is_armed: bool) -> Option<Item> {
        let line = *topic.feed_lines().get(at)?;
        let word = if is_armed {
            CLICK_TO_REMOVE
        } else if self.rundowns.silent.holds(&topic.name, line) {
            SILENT
        } else {
            ""
        };
        Some(Item::new(Feed::parse(line).source().to_uppercase(), word).with_icon(FEED_ICON))
    }

    pub(crate) fn rundown_item(&self, index: usize) -> Option<Item> {
        let topic = self.page_topic()?;
        let is_armed = self.rundowns.armed == Some(index);
        Some(match *self.rundown_page_lines().get(index)? {
            PageLine::Deletion => Item::action(DELETE, THE_RUNDOWN, DELETE_ICON),
            PageLine::Keep => Item::action(KEEP, AS_IT_IS, DROP_ICON),
            PageLine::Made => Item::new(MADE, if topic.is_on { EVERY_EDITION } else { OFF }),
            PageLine::About => Item::new(ABOUT, topic.about.clone()),
            PageLine::Feed(at) => self.feed_item(topic, at, is_armed)?,
            PageLine::Delete if is_armed => Item::action(DELETE, CLICK_AGAIN_TO_DELETE, DELETE_ICON),
            PageLine::Delete => Item::action(DELETE, THE_RUNDOWN, DELETE_ICON),
        })
    }

    pub(super) fn wheel_rundown(&mut self, detents: i32) {
        if self.rundowns.armed.take().is_some() {
            self.deal_lines(List::Rundown, |_| true);
        }
        self.wheel_lines(List::Rundown, detents);
    }

    // Deletes the first rundown a request asks to delete, or keeps it: the next one's page follows, or the list.
    fn settle_deletion(&mut self, is_deleted: bool, now_ns: u64) {
        let Some(name) = self.rundowns.deleting.pop_front() else {
            return;
        };
        let is_changed = is_deleted && self.change_rundowns(Change::Delete(name));
        self.react(if is_changed { Reaction::Nod } else { Reaction::Shake }, now_ns);
        if self.rundowns.deleting.is_empty() {
            self.depart(BACKWARD);
            self.screen = Some(Screen::Rundowns);
        } else {
            self.show_deletion();
        }
        self.note_rundowns_change();
    }

    // A click on a page's line: a deletion a request asks is made or refused; a rundown is switched, and a feed or the rundown itself goes at the second click on its line.
    pub(super) fn act_on_rundown(&mut self, now_ns: u64) {
        let at = self.rundowns.page.selection;
        let line = self.rundown_page_lines().get(at).copied();
        let is_asked = !self.rundowns.deleting.is_empty();
        let Some(name) = self.page_topic().map(|topic| topic.name.clone()) else {
            return;
        };
        let was_armed = self.rundowns.armed.take() == Some(at);
        match line {
            Some(PageLine::Deletion) => self.settle_deletion(true, now_ns),
            Some(PageLine::Keep) => self.settle_deletion(false, now_ns),
            // The page stays as it is laid: its line and the menu's tell the switch.
            Some(PageLine::Made) if !is_asked => {
                let is_on = self.rundowns.kept.get(&name).is_some_and(|topic| topic.is_on);
                self.rundowns.is_changed |= self.rundowns.kept.switch(&name, !is_on);
                self.deal_lines(List::Rundown, |_| true);
                self.redeal_lines(&[Use::Open(Destination::Rundowns)]);
            }
            Some(PageLine::Feed(feed)) if !is_asked && was_armed => {
                if !self.rundowns.kept.remove_feed(&name, feed) {
                    return self.rundowns.page.swing(1.0);
                }
                self.rundowns.is_changed = true;
                self.react(Reaction::Shake, now_ns);
                self.note_rundowns_change();
            }
            Some(PageLine::Delete) if was_armed => {
                self.depart(BACKWARD);
                self.rundowns.is_changed |= self.rundowns.kept.remove(&name);
                self.react(Reaction::Shake, now_ns);
                self.note_rundowns_change();
            }
            Some(PageLine::Feed(_) | PageLine::Delete) if !is_asked => {
                self.rundowns.armed = Some(at);
                self.deal_lines(List::Rundown, |_| true);
            }
            _ => {
                self.deal_lines(List::Rundown, |_| true);
                self.rundowns.page.swing(1.0);
            }
        }
    }

    pub(super) fn view_rundowns<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        let has_status = self.rundowns.request.voice.write_word(now_ns, word);
        let status: &'a str = word;
        let count = self.rundowns.kept.all().len();
        let selection = self.rundowns.list.selection.checked_sub(FIRST_ENTRY).filter(|at| *at < count);
        let words = BarWords {
            status: has_status.then_some(status),
            place: selection.map(|at| (at + 1, count)),
            subject: (count == 0).then_some(NONE_KEPT),
            speech: Some(write_list_speech),
            ..BarWords::default()
        };
        View::Lines { list: List::Rundowns, name: RUNDOWNS, words }
    }

    pub(super) fn view_rundown(&self) -> View<'_> {
        let status = (!self.rundowns.deleting.is_empty()).then_some(CLICK_TO_DELETE);
        let words = BarWords { status, place: self.place_in(List::Rundown), ..BarWords::default() };
        View::Lines { list: List::Rundown, name: &self.rundowns.title, words }
    }
}
