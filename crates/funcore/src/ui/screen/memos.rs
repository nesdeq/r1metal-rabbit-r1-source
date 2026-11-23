use super::action::Action;
use super::bar::BarWords;
use super::tree::{DAYS_ICON, MEMORY_ICON, NEWS_ICON, NOTES_ICON, TRANSLATE_ICON};
use super::{BACKWARD, FORWARD, Stopwatch, View};
use crate::base::text::has_words;
use crate::base::time::stamp::{parse_compact_day, parse_stamp};
use crate::base::time::{Moment, to_duration_text, write_duration};
use crate::domain::cause::Cause;
use crate::domain::chat::memory::fact_day;
use crate::domain::chat::state::{Chore, Lapse};
use crate::domain::chat::tools::Destination;
use crate::domain::memos::{Memo, Request};
use crate::domain::news::state::Report as EditorReport;
use crate::domain::paper::dates::{to_edition_name, to_entry_name};
use crate::domain::paper::markdown;
use crate::domain::paper::names::note_sound_path;
use crate::domain::paper::sheet::{SUMMARY, to_rundown_bar};
use crate::domain::scene::shelf_names;
use crate::domain::scribe::state::Report as ScribeReport;
use crate::domain::shelf::Shelf;
use crate::domain::uses::Use;
use crate::openai::ledger::SummaryKind;
use crate::signal::attachment::Enclosure;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::{Aside, ENTRY_LINES, Item};
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::view::lines::List;
use crate::ui::words::{TRANSCRIBING, cause_word};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const SHORT_LETTERS: usize = 3;
const WORDS: &str = "TRANSCRIPT";
const REMARKS: &str = "CONVERSATION";
const FACT: &str = "FACT";
const RECOLLECTION: &str = "RECOLLECTION";
const FORGET: &str = "FORGET";
const ALWAYS: &str = "always";
const NO_ENTRY: &str = "NONE YET";
const PLAYING: &str = "PLAYING";
const STOP: &str = "STOP";
const DELETE: &str = "DELETE";
const RECORDING: &str = "RECORDING ";
const SUMMARIZING: &str = "SUMMARIZING";
const GATHERING: &str = "GATHERING";
const WRITING: &str = "WRITING";
const PLAY_ICON: &str = icon!("play_arrow");
const STOP_ICON: &str = icon!("stop");
const DELETE_ICON: &str = icon!("delete");
const SEND_ICON: &str = icon!("send");
const SOUND_MARK: &str = icon!("graphic_eq");
const SUMMARY_ICON: &str = icon!("summarize");
const WORDS_ICON: &str = icon!("description");
const REMARKS_ICON: &str = icon!("forum");
pub(super) const ICONS: [&str; 8] =
    [PLAY_ICON, STOP_ICON, DELETE_ICON, SEND_ICON, SUMMARY_ICON, WORDS_ICON, REMARKS_ICON, SOUND_MARK];

const fn first_entry(shelf: Shelf) -> usize {
    match shelf {
        Shelf::Notes | Shelf::Talks => 1,
        Shelf::News | Shelf::Facts | Shelf::Days => 0,
    }
}

// Nothing works on the robot's entries, whose words are never shown.
const fn work_words(shelf: Shelf) -> (&'static str, &'static str) {
    match shelf {
        Shelf::Notes | Shelf::Talks | Shelf::Facts | Shelf::Days => (TRANSCRIBING, SUMMARIZING),
        Shelf::News => (GATHERING, WRITING),
    }
}

const fn icon_of(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Notes => NOTES_ICON,
        Shelf::Talks => TRANSLATE_ICON,
        Shelf::News => NEWS_ICON,
        Shelf::Facts => MEMORY_ICON,
        Shelf::Days => DAYS_ICON,
    }
}

const fn mark_of(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Notes => SOUND_MARK,
        Shelf::Talks | Shelf::News | Shelf::Facts | Shelf::Days => icon_of(shelf),
    }
}

// The stem an entry is named and dated by: a fact's by the day it was learned, else `always`.
fn dating(shelf: Shelf, stem: &str) -> String {
    match shelf {
        Shelf::Facts => fact_day(stem).unwrap_or_else(|| ALWAYS.into()),
        Shelf::Notes | Shelf::Talks | Shelf::News | Shelf::Days => stem.into(),
    }
}

pub(super) fn entry_name(shelf: Shelf, stem: &str) -> String {
    match shelf {
        Shelf::Notes | Shelf::Talks => to_entry_name(stem),
        Shelf::News | Shelf::Facts | Shelf::Days => to_edition_name(&dating(shelf, stem)),
    }
}

fn lapse_of(shelf: Shelf, stem: String) -> Option<Lapse> {
    match shelf {
        Shelf::Facts => Some(Lapse::Fact(stem)),
        Shelf::Days => Some(Lapse::Day(stem)),
        Shelf::Notes | Shelf::Talks | Shelf::News => None,
    }
}

pub(super) fn cut_short(name: &str) -> &str {
    &name[..SHORT_LETTERS.min(name.len())]
}

fn to_brief_name(stem: &str) -> String {
    if let Some(time) = parse_stamp(stem) {
        return format!("{:>2} {} {}", time.day, cut_short(time.month_name()), time.to_time_text());
    }
    match parse_compact_day(stem) {
        Some(day) => format!("{} {:>2} {}", cut_short(day.weekday_name()), day.day, cut_short(day.month_name())),
        None => stem.into(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Text {
    Summary,
    Words,
    // A rundown of an edition, by its place among the day's.
    Rundown(usize),
}

// A text of an entry, where it has words.
pub(super) fn body_of(memo: &Memo, text: Text) -> Option<&str> {
    let body = match text {
        Text::Summary => memo.summary.as_deref(),
        Text::Words => memo.words.as_deref(),
        Text::Rundown(at) => memo.rundowns.get(at).map(|rundown| rundown.1.as_str()),
    };
    body.filter(|body| has_words(body))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PageLine {
    Play,
    Read(Text),
    // A note's sound goes to a conversation of Signal's.
    Send,
    Delete,
}

pub(super) fn day_of(stem: &str) -> Option<Moment> {
    parse_stamp(stem).or_else(|| parse_compact_day(stem))
}

pub(crate) struct Memos {
    pub(super) shelf: Shelf,
    pub(crate) grid: Grid,
    pub(crate) entries: Option<Vec<Memo>>,
    pub(crate) sheet: Grid,
    pub(crate) open_entry: Option<String>,
    open_name: String,
    pub(crate) recording: Stopwatch,
    pub(in crate::ui) playing: Option<String>,
    pub(super) writing: Option<String>,
    pub(super) summarizing: Option<String>,
    pub(super) fault: Option<Cause>,
    pub(super) requests: Vec<Request>,
    // Whether a note is recorded, and the note that plays, as the notes' keeper last told.
    recorder: Option<(bool, Option<String>)>,
    scribe: ScribeReport,
    editor: (Option<String>, Option<String>),
    pub(super) file_changes: u32,
    // The entry the conversation asked to forget, whose page opens at its last line once the list holds it.
    forgetting: Option<String>,
}

impl Memos {
    pub(crate) fn new() -> Memos {
        Memos {
            forgetting: None,
            shelf: Shelf::Notes,
            grid: Grid::empty(),
            entries: None,
            sheet: Grid::empty(),
            open_entry: None,
            open_name: String::new(),
            recording: Stopwatch::default(),
            playing: None,
            writing: None,
            summarizing: None,
            fault: None,
            requests: Vec::new(),
            recorder: None,
            scribe: ScribeReport::default(),
            editor: (None, None),
            file_changes: 0,
        }
    }
}

fn write_memos_speech(ui: &Ui, text: &mut String) {
    let Some(entries) = &ui.memos.entries else {
        return text.push_str("Let me look.");
    };
    let (count, plural) = (entries.len(), if entries.len() == 1 { "" } else { "s" });
    let _ = match (ui.memos.shelf, entries.first()) {
        (Shelf::Notes, None) => text.write_str("No notes yet."),
        (Shelf::Talks, None) => text.write_str("No conversations yet."),
        (Shelf::News, None) => text.write_str("No edition yet."),
        (Shelf::Notes, Some(memo)) => {
            let _ = write!(text, "{count} note{plural}. The newest: ");
            write_duration(text, memo.seconds);
            text.write_str(".")
        }
        (Shelf::Talks, Some(memo)) => match day_of(&memo.stem) {
            Some(day) => write!(text, "{count} conversation{plural}, the newest {} {}.", day.day, day.month_name()),
            None => write!(text, "{count} conversation{plural}."),
        },
        (Shelf::News, Some(memo)) => match (day_of(&memo.stem), ui.today()) {
            (Some(day), Some(today)) if (day.year, day.month, day.day) == today => {
                text.write_str("Today's edition is ready.")
            }
            (Some(day), _) => write!(text, "{count} edition{plural}, the newest {}'s.", day.weekday_name()),
            (None, _) => write!(text, "{count} edition{plural}."),
        },
        (Shelf::Facts, None) => text.write_str("I keep no facts of you yet."),
        (Shelf::Facts, Some(_)) => write!(text, "I keep {count} fact{plural} of you."),
        (Shelf::Days, None) => text.write_str("No days yet."),
        (Shelf::Days, Some(memo)) => match day_of(&memo.stem) {
            Some(day) => write!(text, "{count} day{plural}, the newest {} {}.", day.day, day.month_name()),
            None => write!(text, "{count} day{plural}."),
        },
    };
}

fn write_memo_speech(ui: &Ui, text: &mut String) {
    let Some(memo) = ui.current_memo() else {
        return;
    };
    let shelf = ui.memos.shelf;
    let _ = match (shelf, day_of(&dating(shelf, &memo.stem))) {
        (Shelf::Facts, None) => text.write_str("It always holds."),
        (_, None) => Ok(()),
        (Shelf::Notes, Some(day)) => {
            write_duration(text, memo.seconds);
            write!(text, " long, from {} {}.", day.day, day.month_name())
        }
        (Shelf::Talks, Some(day)) => write!(text, "A conversation of {} {}.", day.day, day.month_name()),
        (Shelf::News, Some(day)) => write!(text, "The edition of {}.", day.weekday_name()),
        (Shelf::Facts, Some(day)) => write!(text, "Learned {} {}.", day.day, day.month_name()),
        (Shelf::Days, Some(day)) => write!(text, "What was said {} {}.", day.day, day.month_name()),
    };
}

impl Ui {
    fn visible_shelf(&self) -> Option<Shelf> {
        match self.screen {
            Some(Screen::Talk) => Some(Shelf::Talks),
            Some(Screen::Memos | Screen::Memo | Screen::Reader) => Some(self.memos.shelf),
            _ => None,
        }
    }

    /// The shelf on the screen, none asleep.
    pub fn shelf(&self) -> Option<Shelf> {
        self.visible_shelf().filter(|_| self.is_awake())
    }

    /// The count of changes to the files of notes and conversations since the start.
    pub const fn file_changes(&self) -> u32 {
        self.memos.file_changes
    }

    /// The requests the screens of notes and conversations made since this was last taken.
    pub fn take_memo_requests(&mut self) -> Vec<Request> {
        core::mem::take(&mut self.memos.requests)
    }

    pub(super) fn current_memo(&self) -> Option<&Memo> {
        let (entries, open_entry) = (self.memos.entries.as_ref()?, self.memos.open_entry.as_ref()?);
        entries.iter().find(|memo| memo.stem == *open_entry)
    }

    // The name and the icon of a text of an entry of the shelf that shows: a rundown's by its own name, under the icon it goes by.
    pub(super) fn text_label(&self, memo: &Memo, text: Text) -> (String, &'static str) {
        let (name, icon) = match (text, self.memos.shelf) {
            (Text::Rundown(at), _) => {
                let name = memo.rundowns.get(at).map_or("", |rundown| rundown.0.as_str());
                return (to_rundown_bar(name), self.kept_rundowns().drawing_of(name));
            }
            (Text::Summary, Shelf::Facts) => (FACT, MEMORY_ICON),
            (Text::Summary, Shelf::Days) => (RECOLLECTION, SUMMARY_ICON),
            (Text::Summary, Shelf::Notes | Shelf::Talks | Shelf::News) => (SUMMARY, SUMMARY_ICON),
            (Text::Words, Shelf::Notes) => (WORDS, WORDS_ICON),
            (Text::Words, Shelf::Talks | Shelf::Facts | Shelf::Days | Shelf::News) => (REMARKS, REMARKS_ICON),
        };
        (name.into(), icon)
    }

    fn page_lines(&self) -> Vec<PageLine> {
        let Some(memo) = self.current_memo() else {
            return Vec::new();
        };
        let play = (self.memos.shelf == Shelf::Notes).then_some(PageLine::Play);
        let delete = (self.memos.shelf != Shelf::News).then_some(PageLine::Delete);
        let rundowns = (0..memo.rundowns.len()).map(Text::Rundown);
        let texts =
            [Text::Summary, Text::Words].into_iter().chain(rundowns).filter(|text| body_of(memo, *text).is_some());
        let send = play.map(|_| PageLine::Send);
        play.into_iter().chain(texts.map(PageLine::Read)).chain(send).chain(delete).collect()
    }

    pub(crate) fn do_entries_last(&self) -> bool {
        self.memos.shelf == Shelf::Notes
    }

    pub(super) fn open_memos(&mut self, shelf: Shelf) {
        if core::mem::replace(&mut self.memos.shelf, shelf) != shelf {
            (self.memos.entries, self.memos.open_entry) = (None, None);
        }
        (self.memos.fault, self.memos.forgetting) = (None, None);
        self.memos.grid.begin();
        self.stand_dial(0);
        self.screen = Some(Screen::Memos);
        if let Some(count) = self.memos.entries.as_ref().map(Vec::len) {
            self.memos.grid.set_items(count + first_entry(shelf));
            self.deal_memos();
        }
        if shelf.is_robots() {
            self.ask_chat(Chore::List(shelf));
        }
    }

    // Opens the list where a click forgets what the conversation was asked to forget.
    pub(super) fn aim_forgetting(&mut self, lapse: Lapse, now_ns: u64) -> Option<Action> {
        let (destination, stem) = match lapse {
            Lapse::Thread => return self.reach(&|content| content.runs(Use::Forget), false, now_ns),
            Lapse::Fact(line) => (Destination::Memory, line),
            Lapse::Day(day) => (Destination::Days, day),
        };
        let action = self.reach(&|content| content.runs(Use::Open(destination)), true, now_ns);
        self.memos.forgetting = Some(stem);
        action
    }

    // The page of the entry the conversation asked to forget, at its last line, once the list holds it.
    fn open_forgetting(&mut self) {
        let Some(stem) = self.memos.forgetting.take().filter(|_| self.screen == Some(Screen::Memos)) else {
            return;
        };
        let at = self.memos.entries.as_ref().and_then(|entries| entries.iter().position(|memo| memo.stem == stem));
        if let Some(at) = at {
            self.memos.grid.choose(at + first_entry(self.memos.shelf));
            self.open_memo(at);
            self.memos.sheet.choose(self.page_lines().len().saturating_sub(1));
        }
    }

    pub(super) fn take_entries(&mut self, shelf: Shelf, memos: Vec<Memo>) -> bool {
        if shelf != self.memos.shelf {
            return false;
        }
        if self.memos.entries.as_ref() == Some(&memos) {
            let screen = self.screen;
            self.open_forgetting();
            return self.screen != screen;
        }
        let is_same = self.memos.entries.as_ref().is_some_and(|entries| {
            entries.len() == memos.len() && entries.iter().zip(&memos).all(|(old, new)| old.stem == new.stem)
        });
        let count = memos.len() + first_entry(shelf);
        let selection = self.page_lines().get(self.memos.sheet.selection).copied();
        self.memos.entries = Some(memos);
        match (self.memos.grid.items, is_same) {
            (None, _) if self.screen == Some(Screen::Memos) => {
                self.memos.grid.begin();
                self.memos.grid.set_items(count);
            }
            (None, _) | (Some(_), true) => {}
            (Some(_), false) => {
                self.memos.grid.redeal();
                self.memos.grid.set_items(count);
            }
        }
        self.deal_memos();
        if self.current_memo().is_some() {
            self.lay_sheet();
            if let Some(line) =
                selection.and_then(|selection| self.page_lines().iter().position(|line| *line == selection))
            {
                self.memos.sheet.choose(line);
            }
            if self.screen == Some(Screen::Reader) {
                self.reflow_reader();
            }
        } else if self.memos.open_entry.take().is_some() && matches!(self.screen, Some(Screen::Memo | Screen::Reader)) {
            self.depart(BACKWARD);
            self.screen = Some(Screen::Memos);
        }
        self.open_forgetting();
        matches!(self.screen, Some(Screen::Memos | Screen::Memo | Screen::Reader))
    }

    /// Takes the notes keeper's report; true if the screen changed.
    #[must_use]
    pub fn take_memos_report(&mut self, mut report: crate::domain::memos::state::Report, now_ns: u64) -> bool {
        let is_listed = report.listing.take().is_some_and(|(shelf, memos)| self.take_entries(shelf, memos));
        let is_tallied = report.tally.take().is_some_and(|shelves| self.set_shelves(shelves));
        self.memos.recorder = Some((report.recording.is_some(), report.playing));
        let is_noted = self.note_kept((report.kept_count, false), report.cause, now_ns);
        is_listed | is_tallied | is_noted
    }

    /// Takes the scribe's report; true if the screen changed.
    #[must_use]
    pub fn take_scribe_report(&mut self, report: ScribeReport, now_ns: u64) -> bool {
        let (count, cause) = (report.kept_count, report.cause);
        self.memos.file_changes = self.memos.file_changes.wrapping_add(report.has_changed.into());
        self.memos.scribe = report;
        self.note_kept((count, true), cause, now_ns)
    }

    /// Takes the editor's report; true if the screen changed.
    #[must_use]
    pub fn take_editor_report(&mut self, report: EditorReport, now_ns: u64) -> bool {
        let cause = report.cause.filter(|_| self.visible_shelf() == Some(Shelf::News));
        self.memos.file_changes = self.memos.file_changes.wrapping_add(report.has_changed.into());
        self.memos.editor = report.days;
        let is_silent_told = report.silent.is_some_and(|silent| self.set_silent_feeds(silent));
        self.note_kept((report.kept_count, true), cause, now_ns) | is_silent_told
    }

    fn note_kept(&mut self, (count, is_written): (u32, bool), cause: Option<Cause>, now_ns: u64) -> bool {
        if count > 0 {
            self.react(Reaction::Nod, now_ns);
        }
        if count > 0 && is_written {
            self.notice(false);
        }
        let has_failed = cause.is_some_and(|cause| self.note_memo_failed(cause, now_ns));
        count > 0 || has_failed
    }

    /// Takes the work on the entries of the visible shelf; true if the screen changed.
    #[must_use]
    pub fn update_memo_work(&mut self, now_ns: u64) -> bool {
        let memos = &self.memos;
        let is_recording = memos.recorder.as_ref().is_some_and(|recorder| recorder.0);
        let playing = memos.recorder.as_ref().and_then(|recorder| recorder.1.as_deref());
        let (writing, summarizing) = match self.visible_shelf() {
            Some(Shelf::News) => (memos.editor.0.as_deref(), memos.editor.1.as_deref()),
            Some(Shelf::Talks) => memos.scribe.names_of(SummaryKind::Talk),
            Some(Shelf::Facts | Shelf::Days) => (None, None),
            Some(Shelf::Notes) | None => memos.scribe.names_of(SummaryKind::Note),
        };
        let is_same = playing == memos.playing.as_deref()
            && writing == memos.writing.as_deref()
            && summarizing == memos.summarizing.as_deref();
        let changed = (!is_same).then(|| [playing, writing, summarizing].map(|stem| stem.map(String::from)));
        self.set_memo_work(is_recording, changed, now_ns)
    }

    // Takes the recording's state, and the stems played, written and summarized where any of them changed.
    fn set_memo_work(&mut self, is_recording: bool, changed: Option<[Option<String>; 3]>, now_ns: u64) -> bool {
        if is_recording == self.memos.recording.start_ns.is_some() && changed.is_none() {
            return false;
        }
        if is_recording != self.memos.recording.start_ns.is_some() {
            self.memos.recording.start_ns = is_recording.then_some(now_ns);
            if is_recording && self.memos.shelf == Shelf::Notes && self.memos.grid.items.is_some() {
                self.memos.grid.choose(0);
            }
        }
        if let Some(stems) = changed {
            [self.memos.playing, self.memos.writing, self.memos.summarizing] = stems;
        }
        self.deal_memos();
        self.deal_sheet();
        matches!(self.screen, Some(Screen::Memos | Screen::Memo))
    }

    fn note_memo_failed(&mut self, cause: Cause, now_ns: u64) -> bool {
        self.memos.fault = Some(cause);
        self.react(Reaction::Slump, now_ns);
        match self.screen {
            Some(Screen::Memos) => self.memos.grid.swing(1.0),
            Some(Screen::Memo) => self.memos.sheet.swing(1.0),
            _ => {}
        }
        matches!(self.screen, Some(Screen::Memos | Screen::Memo))
    }

    fn does_recording_show(&self) -> bool {
        self.memos.shelf == Shelf::Notes && self.memos.recording.start_ns.is_some()
    }

    pub(crate) fn is_hearing_note(&self) -> bool {
        self.screen == Some(Screen::Memos) && self.does_recording_show()
    }

    fn entry_item(&self, memo: &Memo) -> Item {
        let (shelf, stem) = (self.memos.shelf, memo.stem.as_str());
        let length = if shelf == Shelf::Notes { to_duration_text(memo.seconds) } else { String::new() };
        let first_rundown = memo.rundowns.first().map(|rundown| rundown.1.as_str());
        let body = memo.summary.as_deref().or(memo.words.as_deref()).or(first_rundown).unwrap_or_default();
        let (writing_word, summary_word) = work_words(shelf);
        let text = if self.memos.writing.as_deref() == Some(stem) {
            writing_word.into()
        } else if self.memos.summarizing.as_deref() == Some(stem) {
            summary_word.into()
        } else {
            markdown::to_opening(body, ENTRY_LINES * self.line_room(List::Memos) + 1).trim().into()
        };
        let item = Item::entry(entry_name(shelf, stem), to_brief_name(&dating(shelf, stem)), length, text);
        item.with_icon(icon_of(shelf)).with_mark(mark_of(shelf))
    }

    pub(crate) fn memos_item(&self, index: usize) -> Option<Item> {
        if index >= self.memos.grid.items.unwrap_or(0) {
            return None;
        }
        let shelf = self.memos.shelf;
        Some(match (index.checked_sub(first_entry(shelf)), shelf) {
            (None, Shelf::Notes) if self.memos.recording.start_ns.is_some() => {
                Item::new(STOP, "AND KEEP THE NOTE").with_recording().with_aside(Aside::Meter)
            }
            (None, Shelf::Notes) => Item::new("RECORD", "A NEW NOTE").with_recording(),
            (None, Shelf::Talks) => Item::lead("TRANSLATE", "A NEW CONVERSATION, BY OPENAI").with_icon(TRANSLATE_ICON),
            (None, Shelf::News | Shelf::Facts | Shelf::Days) => return None,
            (Some(at), _) => self.entry_item(self.memos.entries.as_ref()?.get(at)?),
        })
    }

    fn deal_memos(&mut self) {
        self.deal_lines(List::Memos, |_| true);
    }

    pub(super) fn pick_memo(&mut self) {
        if self.memos.grid.items.is_none() {
            return;
        }
        self.memos.fault = None;
        let (shelf, selection) = (self.memos.shelf, self.memos.grid.selection);
        match (selection.checked_sub(first_entry(shelf)).filter(|_| !self.does_recording_show()), shelf) {
            (None, Shelf::Notes) => self.memos.requests.push(Request::Record),
            (None, Shelf::Talks) => self.open_talk(),
            (None, Shelf::News | Shelf::Facts | Shelf::Days) => {}
            (Some(at), _) => self.open_memo(at),
        }
    }

    pub(super) fn wheel_memos(&mut self, detents: i32) {
        if self.does_recording_show() {
            return self.memos.grid.swing(detents.signum() as f32);
        }
        self.memos.fault = None;
        self.wheel_lines(List::Memos, detents);
    }

    fn open_memo(&mut self, at: usize) {
        let Some(stem) = self.memos.entries.as_ref().and_then(|entries| entries.get(at)).map(|memo| memo.stem.clone())
        else {
            return;
        };
        self.depart(FORWARD);
        self.memos.open_name = entry_name(self.memos.shelf, &stem);
        self.memos.open_entry = Some(stem);
        self.memos.sheet.begin();
        self.lay_sheet();
        self.stand_dial(0);
        self.screen = Some(Screen::Memo);
    }

    fn lay_sheet(&mut self) {
        self.memos.sheet.set_items(self.page_lines().len());
        self.deal_sheet();
    }

    pub(crate) fn sheet_item(&self, index: usize) -> Option<Item> {
        let memo = self.current_memo()?;
        let (lines, room) = (self.page_lines(), self.line_room(List::Memo));
        let has_summary = lines.contains(&PageLine::Read(Text::Summary));
        let has_text = lines.iter().any(|line| matches!(line, PageLine::Read(_)));
        let stem = Some(memo.stem.as_str());
        Some(match (*lines.get(index)?, self.memos.shelf) {
            (PageLine::Play, _) if self.memos.playing.as_deref() == stem => {
                Item::new(STOP, PLAYING).with_icon(STOP_ICON)
            }
            (PageLine::Play, _) => Item::new("PLAY", to_duration_text(memo.seconds)).with_icon(PLAY_ICON),
            (PageLine::Read(text), _) => {
                let (name, icon) = self.text_label(memo, text);
                let opening = markdown::to_opening(body_of(memo, text)?, room + 1);
                Item { text: opening.trim().into(), ..Item::lead(name, "") }.with_icon(icon)
            }
            (PageLine::Send, _) => Item::action("SEND", "ON SIGNAL", SEND_ICON),
            (PageLine::Delete, Shelf::Notes) if has_text => {
                Item::action(DELETE, "THE NOTE AND WHAT IS WRITTEN OF IT", DELETE_ICON)
            }
            (PageLine::Delete, Shelf::Notes) => Item::action(DELETE, "THE NOTE", DELETE_ICON),
            (PageLine::Delete, Shelf::Talks) if has_summary => {
                Item::action(DELETE, "THE CONVERSATION AND ITS SUMMARY", DELETE_ICON)
            }
            (PageLine::Delete, Shelf::Talks) => Item::action(DELETE, "THE CONVERSATION", DELETE_ICON),
            (PageLine::Delete, Shelf::Facts) => Item::action(FORGET, "THE FACT", DELETE_ICON),
            (PageLine::Delete, Shelf::Days) if has_summary => {
                Item::action(FORGET, "THE DAY AND ITS RECOLLECTION", DELETE_ICON)
            }
            (PageLine::Delete, Shelf::Days) => Item::action(FORGET, "THE DAY", DELETE_ICON),
            (PageLine::Delete, Shelf::News) => return None,
        })
    }

    fn deal_sheet(&mut self) {
        if self.current_memo().is_some() {
            self.deal_lines(List::Memo, |_| true);
        }
    }

    pub(super) fn wheel_memo(&mut self, detents: i32) {
        self.memos.fault = None;
        self.wheel_lines(List::Memo, detents);
    }

    pub(super) fn act_on_memo(&mut self, now_ns: u64) {
        let Some(stem) = self.memos.open_entry.clone() else {
            return;
        };
        self.memos.fault = None;
        let shelf = self.memos.shelf;
        match self.page_lines().get(self.memos.sheet.selection).copied() {
            Some(PageLine::Play) => self.memos.requests.push(Request::Play(stem)),
            Some(PageLine::Read(text)) => self.open_reader(text),
            Some(PageLine::Send) => {
                let seconds = self.current_memo().map_or(0, |memo| memo.seconds);
                self.carry_to_signal(Enclosure::note(note_sound_path(&stem)), seconds, now_ns);
            }
            Some(PageLine::Delete) => {
                self.depart(BACKWARD);
                if let Some(entries) = &mut self.memos.entries {
                    entries.retain(|memo| memo.stem != stem);
                    self.memos.grid.redeal();
                    self.memos.grid.set_items(entries.len() + first_entry(shelf));
                }
                self.deal_memos();
                (self.screen, self.memos.open_entry) = (Some(Screen::Memos), None);
                self.react(Reaction::Shake, now_ns);
                self.memos.requests.extend(shelf.summary_kind().map(|kind| Request::Delete(kind, stem.clone())));
                if let Some(lapse) = lapse_of(shelf, stem) {
                    self.ask_chat(Chore::Forget(lapse));
                }
            }
            None => {}
        }
    }

    fn edition_status(&self) -> Option<&'static str> {
        let (writing_word, summary_word) = work_words(self.memos.shelf);
        match (self.memos.shelf, &self.memos.writing, &self.memos.summarizing) {
            (Shelf::News, Some(_), _) => Some(writing_word),
            (Shelf::News, None, Some(_)) => Some(summary_word),
            _ => None,
        }
    }

    pub(super) fn view_memos<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        let recording = self.memos.recording.start_ns.filter(|_| self.does_recording_show());
        let (first, (plural, singular)) = (first_entry(self.memos.shelf), shelf_names(self.memos.shelf));
        let entries = self.memos.grid.items.map(|count| count.saturating_sub(first));
        let selection = self.memos.grid.selection.checked_sub(first);
        let is_status = match (self.memos.fault.map(cause_word), recording, self.edition_status()) {
            (Some(status), ..) | (None, None, Some(status)) => {
                word.push_str(status);
                true
            }
            (None, Some(start_ns), _) => {
                word.push_str(RECORDING);
                write_duration(word, super::seconds_since(start_ns, now_ns));
                true
            }
            (None, None, None) => {
                let _ = match (entries, selection) {
                    (Some(0), _) => word.write_str(NO_ENTRY),
                    (Some(1), None) => write!(word, "1 {singular}"),
                    (Some(entries), None) => write!(word, "{entries} {plural}"),
                    _ => Ok(()),
                };
                false
            }
        };
        let word: &'a str = word;
        let place = entries.zip(selection).filter(|entry| entry.0 > 0).map(|(entries, at)| (at + 1, entries));
        let words = BarWords {
            status: is_status.then_some(word),
            subject: Some(word).filter(|word| !is_status && !word.is_empty()),
            place,
            speech: Some(write_memos_speech),
            ..BarWords::default()
        };
        View::Lines { list: List::Memos, name: plural, words }
    }

    pub(super) fn view_memo(&self) -> View<'_> {
        let (stem, (writing_word, summary_word)) = (self.memos.open_entry.as_deref(), work_words(self.memos.shelf));
        let status = match self.memos.fault {
            Some(cause) => Some(cause_word(cause)),
            None if stem.is_some() && self.memos.playing.as_deref() == stem => Some(PLAYING),
            None if stem.is_some() && self.memos.writing.as_deref() == stem => Some(writing_word),
            None if stem.is_some() && self.memos.summarizing.as_deref() == stem => Some(summary_word),
            None => None,
        };
        let subject = Some(self.memos.open_name.as_str());
        let words = BarWords { status, subject, speech: Some(write_memo_speech), ..BarWords::default() };
        View::Lines { list: List::Memo, name: shelf_names(self.memos.shelf).1, words }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_name() {
        assert_eq!(
            [
                to_brief_name("20261012_190328"),
                to_brief_name("20261009_174500"),
                to_brief_name("20261013"),
                to_brief_name("memo")
            ],
            ["12 oct 19:03", " 9 oct 17:45", "tue 13 oct", "memo"]
        );
    }
}
