use super::action::Action;
use super::bar::BarWords;
use super::voice::{RequestList, Voice, VoiceId};
use super::{BACKWARD, FORWARD, Screen, View};
use crate::base::text::has_words;
use crate::domain::camera::Shot;
use crate::domain::chat::MAX_TEXT_CHARS;
use crate::domain::chat::state::{Chore, Lapse, Milestone, Report};
use crate::domain::chat::tools::{Act, Asked, Destination, Readout};
use crate::domain::chat::transcript::{Entry, EntryKind};
use crate::domain::gallery::Kind;
use crate::domain::memos::Request;
use crate::domain::paper::markdown::{self, Blocks};
use crate::domain::paper::rows::{self, Row};
use crate::domain::scene::MUTED;
use crate::domain::settings::BRIGHTNESS_SCALE;
use crate::domain::shelf::Shelf;
use crate::domain::signal::robot::Question;
use crate::domain::uses::Use;
use crate::domain::voice::{Bid, Phase};
use crate::gfx::motion::Edge;
use crate::hw::sound::volume;
use crate::ui::keys::{Keyboard, raise, ready, take_typed};
use crate::ui::robot::Reaction;
use crate::ui::textroom::TextRoom;
use crate::ui::textroom::talk::{Line, LineKind, to_speaker};
use crate::ui::view::rail::Stand;
use crate::ui::words::{HOLD_TO_TALK, VOICE};
use crate::ui::{HOLD_TO_TALK_NS, Ui};
use alloc::string::String;
use alloc::vec::Vec;

const NAMELESS: &str = "ROBOT";
const HOLD_THE_BUTTON: &str = "HOLD THE BUTTON TO TALK";
const SEARCHED: &str = "searched online";
const SHOWN: &str = "photo shown";
const ATTACHED: &str = "photo attached";

/// What the interface does with a readout the model asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reading {
    // The interface's own facts, told.
    Told(String),
    // A question for Signal's thread.
    Ask(Question),
}

pub(crate) struct Chat {
    days: Vec<(String, Vec<Entry>)>,
    may_have_older: bool,
    has_asked_older: bool,
    live: Option<Entry>,
    pub(crate) lines: Vec<Line>,
    pub(crate) live_start: usize,
    entries_height: i32,
    pub(in crate::ui) height: i32,
    pub(in crate::ui) room: TextRoom<i32>,
    pub(in crate::ui) is_below: bool,
    pub(crate) voice: Voice,
    pub(crate) is_muted: bool,
    pub(crate) name: String,
    pub(crate) speaker: String,
    pub(in crate::ui) keys: Keyboard,
    pub(crate) band: Edge,
    bids: Vec<Bid<Chore>>,
    acts: Vec<Act>,
    pub(in crate::ui) stale_row: Option<i32>,
    // The screen a hold or a photo for the robot opened the conversation over, which going back returns to.
    opened_from: Option<Screen>,
    // Where the rail stood as a hold or a photo for the robot opened the conversation, which going back returns it to.
    rail_from: Option<Stand>,
    has_photo: bool,
}

impl Chat {
    pub(crate) fn new(keys: Keyboard) -> Chat {
        Chat {
            days: Vec::new(),
            may_have_older: true,
            has_asked_older: false,
            live: None,
            lines: Vec::new(),
            live_start: 0,
            entries_height: 0,
            height: 0,
            room: TextRoom::default(),
            is_below: false,
            voice: Voice::new(),
            is_muted: false,
            name: String::from(NAMELESS),
            speaker: to_speaker(NAMELESS),
            keys,
            band: Edge::still(0.0),
            bids: Vec::new(),
            acts: Vec::new(),
            stale_row: None,
            opened_from: None,
            rail_from: None,
            has_photo: false,
        }
    }
}

impl Ui {
    pub(crate) fn does_chat_show(&self) -> bool {
        self.screen == Some(Screen::Chat)
    }

    /// The bids the conversation's screen made since this was last taken.
    pub fn take_chat_bids(&mut self) -> Vec<Bid<Chore>> {
        core::mem::take(&mut self.chat.bids)
    }

    pub(super) fn open_chat(&mut self) {
        (self.screen, self.chat.opened_from, self.chat.rail_from) = (Some(Screen::Chat), None, None);
        (self.chat.room.at, self.chat.is_below) = (0, false);
        (self.chat.room.scroll, self.chat.band) =
            (Edge::still(self.chat_aim()), Edge::still(self.chat_band_height() as f32));
        self.chat.room.plate.make_room(self.ruler.room_size());
        if self.chat.is_muted {
            ready(&mut self.chat.keys, MAX_TEXT_CHARS);
        }
        self.chat.bids.push(Bid::Job(Chore::Open));
    }

    /// Takes the mute state and the robot's name; true if the screen changed.
    #[must_use]
    pub fn set_chat_identity(&mut self, is_muted: bool, name: Option<&str>) -> bool {
        let new_name = name.map_or_else(|| String::from(NAMELESS), str::to_uppercase);
        let has_mute_changed = core::mem::replace(&mut self.chat.is_muted, is_muted) != is_muted;
        let is_renamed = core::mem::replace(&mut self.chat.name, new_name) != self.chat.name;
        let mut is_hero_dealt = false;
        if is_renamed {
            self.chat.speaker = to_speaker(&self.chat.name);
            is_hero_dealt = self.deal_hero();
            self.reflow_speakers();
        }
        if has_mute_changed {
            self.lay_chat_live();
        }
        if has_mute_changed && self.does_chat_show() {
            if is_muted {
                ready(&mut self.chat.keys, MAX_TEXT_CHARS);
            }
            self.chat.room.scroll = Edge::still(self.chat_aim());
        }
        (has_mute_changed || is_renamed) && self.does_chat_show() || is_hero_dealt
    }

    /// Takes the conversation's report; returns if the screen changed and what the interface does with the readout asked for.
    #[must_use]
    pub fn take_chat_report(&mut self, report: Report, now_ns: u64) -> (bool, Option<Reading>) {
        let mut does_redraw = report.phase.is_some_and(|phase| self.set_chat_phase(phase, now_ns));
        if let Some((is_muted, name)) = report.identity {
            does_redraw |= self.set_chat_identity(is_muted, name.as_deref());
        }
        for (day, entries) in report.days {
            does_redraw |= self.set_chat_day(&day, entries);
        }
        if report.has_reached_oldest {
            (self.chat.may_have_older, self.chat.has_asked_older) = (false, false);
        }
        if let Some(live) = report.current {
            does_redraw |= self.set_chat_live(live, now_ns);
        }
        if let Some(has_photo) = report.has_photo {
            does_redraw |= self.set_chat_photo(has_photo);
        }
        for milestone in &report.milestones {
            match milestone {
                Milestone::Wake if self.does_robot_live() => self.robot.first_light(now_ns),
                Milestone::Wake => {}
                Milestone::Name => self.react(Reaction::Admire, now_ns),
                Milestone::Enough => self.react(Reaction::Cheer, now_ns),
            }
        }
        does_redraw |= !report.milestones.is_empty();
        if let Some((shelf, memos)) = report.listing {
            does_redraw |= self.take_entries(shelf, memos);
        }
        for day in &report.forgotten {
            does_redraw |= self.drop_chat_day(day);
        }
        if self.is_awake() {
            let mut acts = report.acts;
            if let Some(sleep) = acts.iter().position(|act| matches!(act, Act::Sleep)) {
                acts.truncate(sleep + 1);
            }
            self.chat.acts.extend(acts);
        }
        let readout = report.readout.map(|asked| self.take_asked(asked));
        does_redraw |= readout.as_ref().is_some_and(|readout| readout.1);
        (does_redraw, readout.map(|readout| readout.0))
    }

    fn drop_chat_day(&mut self, day: &str) -> bool {
        let count = self.chat.days.len();
        self.chat.days.retain(|known| known.0 != day);
        let is_dropped = self.chat.days.len() != count;
        if is_dropped {
            self.lay_chat();
            self.chat.room.at = self.chat.room.at.min(self.chat_max_scroll());
        }
        is_dropped && self.does_chat_show()
    }

    // What the interface does with what the model asked for, and whether the screen changed by it.
    fn take_asked(&mut self, asked: Asked) -> (Reading, bool) {
        match asked {
            Asked::Device(Readout::Battery) => (Reading::Told(self.battery_facts().to_text()), false),
            Asked::Device(Readout::Bell) => (Reading::Told(self.bell_for_model()), false),
            Asked::Signal(question) => (Reading::Ask(question), false),
            Asked::Bell(change) => {
                let (told, is_changed) = self.change_bell(change);
                (Reading::Told(told), is_changed)
            }
        }
    }

    /// Takes the acts the conversation asked of the device.
    pub fn take_acts(&mut self) -> Vec<Act> {
        core::mem::take(&mut self.chat.acts)
    }

    fn set_chat_phase(&mut self, phase: Phase, now_ns: u64) -> bool {
        if !self.take_phase(VoiceId::Chat, phase, now_ns) {
            return false;
        }
        self.lay_chat_live();
        self.does_chat_show()
    }

    /// Takes the entries of a day; true if the screen changed.
    #[must_use]
    pub fn set_chat_day(&mut self, day: &str, entries: Vec<Entry>) -> bool {
        let at = self.chat.days.iter().position(|known| known.0 == day);
        let is_last = at.is_some_and(|at| at + 1 == self.chat.days.len());
        let previous = self.chat.height;
        match at {
            Some(at) if self.chat.days[at].1 == entries => return false,
            Some(at) if is_last && entries.starts_with(&self.chat.days[at].1) => {
                let new_entries = entries[self.chat.days[at].1.len()..].to_vec();
                self.chat.days[at].1 = entries;
                self.add_chat_entries(&new_entries);
            }
            Some(at) => {
                self.chat.days[at].1 = entries;
                self.lay_chat();
            }
            None => {
                let place = self.chat.days.partition_point(|known| known.0.as_str() < day);
                let is_first = place == 0 && !self.chat.days.is_empty();
                self.chat.days.insert(place, (String::from(day), entries));
                self.lay_chat();
                if is_first {
                    self.chat.has_asked_older = false;
                    self.chat.room.scroll.at += (self.chat.height - previous) as f32;
                    return self.does_chat_show();
                }
            }
        }
        self.note_chat_growth(previous);
        self.deal_hero()
    }

    fn set_chat_live(&mut self, live: Option<Entry>, now_ns: u64) -> bool {
        if live == self.chat.live {
            return false;
        }
        let previous = self.chat.height;
        self.chat.live = live;
        self.lay_chat_live();
        self.note_chat_growth(previous);
        self.used_at = Some(now_ns);
        self.does_chat_show()
    }

    fn set_chat_photo(&mut self, has_photo: bool) -> bool {
        if core::mem::replace(&mut self.chat.has_photo, has_photo) == has_photo {
            return false;
        }
        let previous = self.chat.height;
        self.lay_chat_live();
        self.note_chat_growth(previous);
        self.does_chat_show()
    }

    /// Runs an act of the conversation.
    pub fn do_act(&mut self, act: Act, now_ns: u64) -> Option<Action> {
        let is_held = self.is_key_held();
        match act {
            Act::Open(_)
            | Act::Enter(_)
            | Act::RecordNote
            | Act::Translate
            | Act::Theme(_)
            | Act::Forget(_)
            | Act::Draft(..)
            | Act::Rundowns(_)
                if is_held =>
            {
                None
            }
            Act::Open(destination) => self.reach(&|content| content.runs(Use::Open(destination)), true, now_ns),
            Act::Enter(menu) => self.enter_menu(menu, now_ns),
            Act::Draft(peer, words) => self.open_draft(peer, words.0, now_ns),
            Act::Rundowns(words) => {
                let action = self.reach(&|content| content.runs(Use::Open(Destination::Rundowns)), true, now_ns);
                self.ask_rundowns(words.0);
                action
            }
            Act::Change(setting, value) => self.change_setting(setting, value, is_held, now_ns),
            Act::Forget(lapse) => self.aim_forgetting(lapse, now_ns),
            Act::RecordNote => {
                let action = self.reach(&|content| content.runs(Use::Open(Destination::Notes)), true, now_ns);
                if self.screen == Some(Screen::Memos)
                    && self.memos.shelf == Shelf::Notes
                    && self.memos.recording.start_ns.is_none()
                {
                    self.memos.requests.push(Request::Record);
                }
                action
            }
            Act::Translate => {
                let action = self.reach(&|content| content.runs(Use::Open(Destination::Translations)), true, now_ns);
                self.open_talk();
                action
            }
            Act::Brightness(percent) => {
                self.settings.brightness = BRIGHTNESS_SCALE.nearest(u32::from(percent));
                self.redeal_lines(&[Use::Open(Destination::Brightness)]);
                None
            }
            Act::Volume(percent) => {
                self.settings.volume = volume::SCALE.nearest(u32::from(percent));
                self.redeal_lines(&[Use::Open(Destination::Volume)]);
                None
            }
            Act::Sounds(percent) => {
                self.settings.sounds = volume::SCALE.nearest(u32::from(percent));
                self.redeal_lines(&[Use::Open(Destination::Volume)]);
                None
            }
            Act::Colours(at) => {
                if at != self.settings.palette() {
                    self.take_palette(at, now_ns);
                }
                None
            }
            Act::Offset(offset) => {
                let _ = self.set_offset(offset);
                None
            }
            Act::Theme(theme) => {
                if theme != self.settings.theme {
                    self.settings.theme = theme;
                    self.relayout();
                    if self.does_chat_show() {
                        // What the conversation opened over has given way to home with every other screen.
                        (self.chat.opened_from, self.chat.rail_from) = (None, None);
                    } else {
                        self.theme_home();
                    }
                    self.recolour();
                    self.chat.band = Edge::still(self.chat_band_aim());
                    self.react(Reaction::Admire, now_ns);
                }
                None
            }
            Act::Home => {
                self.go_home(now_ns);
                None
            }
            Act::Sleep => Some(self.fall_asleep(now_ns)),
        }
    }

    pub(super) fn forget_thread(&mut self, now_ns: u64) {
        if self.chat.voice.phase.is_working() {
            return self.swing_line();
        }
        self.chat.bids.push(Bid::Job(Chore::Forget(Lapse::Thread)));
        self.react(Reaction::Shake, now_ns);
    }

    pub(super) fn ask_chat(&mut self, chore: Chore) {
        self.chat.bids.push(Bid::Job(chore));
    }

    pub(crate) fn chat_last_words(&self) -> String {
        let last = self.chat.days.iter().rev().find_map(|day| {
            day.1.iter().rev().find(|entry| entry.kind == EntryKind::Robot).map(|entry| entry.text.as_str())
        });
        last.map(|text| markdown::to_opening(text, MAX_TEXT_CHARS)).unwrap_or_default()
    }

    pub(crate) fn may_chat_prompt(&self, is_muted: bool) -> bool {
        !is_muted && self.chat.voice.phase == Phase::Idle && self.chat.live.is_none()
    }

    fn lay_chat_entry(&self, entry: &Entry, lines: &mut Vec<Line>, top: &mut i32) {
        if entry.kind == EntryKind::Sources {
            return;
        }
        let (speaker, talk) = (self.chat.speaker.as_str(), self.talk_form());
        let (cells, heading_cells) = talk.cells(LineKind::Robot, speaker);
        talk.push(lines, top, LineKind::Robot, Row::gap());
        match entry.kind {
            EntryKind::User(_) => {
                for line in rows::wrap_lines(&entry.text, talk.cells(LineKind::You, speaker).0, 0) {
                    talk.push(lines, top, LineKind::You, Row::plain(line, false));
                }
            }
            EntryKind::Robot => {
                let mut previous = None;
                for (block, _) in Blocks::new(&entry.text, 0) {
                    let mut block_rows = Vec::new();
                    rows::lay_block(&block, previous, cells, heading_cells, &mut block_rows);
                    previous = Some(block.is_item());
                    block_rows.into_iter().for_each(|row| talk.push(lines, top, LineKind::Robot, row));
                }
            }
            EntryKind::Searches => talk.push(lines, top, LineKind::Aside, Row::plain(String::from(SEARCHED), true)),
            EntryKind::Shown => talk.push(lines, top, LineKind::Aside, Row::plain(String::from(SHOWN), true)),
            EntryKind::Sources => {}
        }
    }

    fn note_chat_stale(&mut self, row: i32) {
        self.chat.stale_row = Some(self.chat.stale_row.map_or(row, |stale_row| stale_row.min(row)));
    }

    pub(crate) fn reflow_chat(&mut self) {
        self.lay_chat();
        (self.chat.room.at, self.chat.is_below, self.chat.room.plate.key) = (0, false, None);
        self.chat.room.scroll = Edge::still(self.chat_aim());
    }

    pub(crate) fn lay_chat(&mut self) {
        let (mut lines, mut top) = (core::mem::take(&mut self.chat.lines), 0);
        lines.clear();
        let days = core::mem::take(&mut self.chat.days);
        for (day, entries) in &days {
            self.talk_form().lay_day(day, &mut lines, &mut top);
            entries.iter().for_each(|entry| self.lay_chat_entry(entry, &mut lines, &mut top));
        }
        self.chat.days = days;
        (self.chat.live_start, self.chat.entries_height, self.chat.lines) = (lines.len(), top, lines);
        self.note_chat_stale(0);
        self.lay_chat_live();
    }

    fn add_chat_entries(&mut self, new_entries: &[Entry]) {
        let mut lines = core::mem::take(&mut self.chat.lines);
        lines.truncate(self.chat.live_start);
        let mut top = self.chat.entries_height;
        self.note_chat_stale(top);
        new_entries.iter().for_each(|entry| self.lay_chat_entry(entry, &mut lines, &mut top));
        (self.chat.live_start, self.chat.entries_height, self.chat.lines) = (lines.len(), top, lines);
        self.lay_chat_live();
    }

    fn lay_chat_live(&mut self) {
        let may_prompt = self.may_chat_prompt(self.chat.is_muted);
        let mut lines = core::mem::take(&mut self.chat.lines);
        let previous = lines.split_off(self.chat.live_start);
        let mut top = self.chat.entries_height;
        let live = self.chat.live.take();
        let talk = self.talk_form();
        if self.chat.has_photo {
            talk.push(&mut lines, &mut top, LineKind::Robot, Row::gap());
            talk.push(&mut lines, &mut top, LineKind::Aside, Row::plain(String::from(ATTACHED), true));
        }
        let is_heard = live.as_ref().is_some_and(|entry| has_words(&entry.text));
        if let Some(entry) = live.as_ref().filter(|_| is_heard) {
            self.lay_chat_entry(entry, &mut lines, &mut top);
        }
        if self.chat.voice.is_listening() && !is_heard {
            talk.push_meter(&mut lines, &mut top);
        }
        if may_prompt {
            talk.push_prompt(&mut lines, &mut top);
        }
        let current = &lines[self.chat.live_start..];
        let same_count = previous.iter().zip(current).take_while(|entry| entry.0 == entry.1).count();
        if same_count < previous.len().max(current.len()) {
            let tops = previous.get(same_count).into_iter().chain(current.get(same_count)).map(|line| line.top);
            tops.min().into_iter().for_each(|row| self.note_chat_stale(row));
        }
        self.chat.live = live;
        (self.chat.lines, self.chat.height) = (lines, top);
    }

    fn note_chat_growth(&mut self, previous: i32) {
        if self.chat.room.at > 0 && self.chat.height != previous {
            self.chat.room.at = (self.chat.room.at + self.chat.height - previous).clamp(0, self.chat_max_scroll());
            self.chat.is_below |= self.chat.height > previous;
        }
    }

    pub(super) fn note_chat_wheeled(&mut self, detents: i32) {
        if detents > 0
            && self.chat.room.at == self.chat_max_scroll()
            && self.chat.may_have_older
            && !self.chat.has_asked_older
            && let Some((oldest, _)) = self.chat.days.first()
        {
            self.chat.bids.push(Bid::Job(Chore::PreviousDay(oldest.clone())));
            self.chat.has_asked_older = true;
        }
        self.chat.is_below &= self.chat.room.at > 0;
    }

    // True where the held button talks to the robot: on home, the stages and menus and every screen where a hold does nothing else.
    fn holds_to_talk(&self, screen: Option<Screen>) -> bool {
        match screen {
            None => true,
            Some(
                Screen::Finder(Kind::Video)
                | Screen::Talk
                | Screen::Keyboard
                | Screen::Pin
                | Screen::Key
                | Screen::Safe
                | Screen::Ring
                | Screen::Call
                | Screen::Incoming
                | Screen::Chat
                | Screen::Pokedex
                | Screen::Web
                | Screen::Conversation,
            ) => false,
            Some(Screen::Memos | Screen::Memo) => {
                self.memos.recording.start_ns.is_none() && self.memos.playing.is_none()
            }
            Some(_) => true,
        }
    }

    // The screen whose voice a held press records for, none where a hold talks to nothing: the screen's own where it has one, the Pokédex or the browser the rail opened, the bell or the rundowns where their list shows, else the conversation.
    pub(super) fn talk_screen(&self) -> Option<Screen> {
        let entered = self.entered_screen();
        match (entered, self.screen) {
            (Some(own @ (Screen::Chat | Screen::Pokedex | Screen::Web | Screen::Conversation)), _) => Some(own),
            _ if !self.holds_to_talk(entered) => None,
            (_, Some(shown @ (Screen::Pokedex | Screen::Web | Screen::Bell | Screen::Rundowns))) => Some(shown),
            _ => Some(Screen::Chat),
        }
    }

    // Whether that voice records: the conversation's unmuted, the Pokédex's, the browser's and a Signal conversation's with their keys down, a list's always.
    fn does_hear(&self, screen: Screen) -> bool {
        match screen {
            Screen::Pokedex => !self.pokedex.is_typing,
            Screen::Web => !self.web.is_typing,
            Screen::Conversation => !self.signal.is_typing,
            Screen::Bell | Screen::Rundowns => true,
            _ => !self.chat.is_muted,
        }
    }

    pub(super) fn press_home(&mut self, screen: Screen, now_ns: u64) {
        self.talk_start = Some((now_ns, screen));
        match (screen, RequestList::of(screen)) {
            _ if !self.does_hear(screen) => {}
            (_, Some(list)) => self.overhear_request(list),
            (Screen::Pokedex, None) => self.pokedex.bids.push(Bid::Overhear),
            (Screen::Web, None) => self.web.bids.push(Bid::Overhear),
            (Screen::Conversation, None) => self.signal.bids.push(Bid::Overhear),
            (_, None) => self.chat.bids.push(Bid::Overhear),
        }
    }

    // Opens the conversation over what shows, which going back returns to, the rail on its line; the one the rail opened is entered.
    fn open_chat_over(&mut self, now_ns: u64) {
        if self.does_chat_show() {
            return self.enter_rail_screen();
        }
        let opened_from = self.screen;
        self.depart(FORWARD);
        self.open_chat();
        let rail_from = self.stand_rail_on_chat(now_ns);
        (self.chat.opened_from, self.chat.rail_from) = (opened_from, rail_from);
    }

    fn hold_home(&mut self, screen: Screen, now_ns: u64) {
        let press_ns = self.talk_start.map_or(now_ns, |(start_ns, _)| start_ns);
        (self.talk_start, self.hold_start) = (None, None);
        self.squeeze(false, now_ns);
        let is_heard = self.does_hear(screen);
        match (screen, RequestList::of(screen)) {
            // A list that takes a request stays: its first line takes what is said.
            (_, Some(list)) => {
                self.enter_rail_screen();
                self.press_request(list);
            }
            (Screen::Pokedex, None) => {
                self.enter_rail_screen();
                if is_heard {
                    self.press_pokedex(now_ns);
                }
            }
            // The browser tells a hold from a click by the press's beginning.
            (Screen::Web, None) => {
                self.enter_rail_screen();
                if is_heard {
                    self.press_web(press_ns);
                }
            }
            // A Signal conversation tells a hold from a click by the press's beginning too.
            (Screen::Conversation, None) => {
                if is_heard {
                    self.press_conversation(press_ns);
                }
            }
            (_, None) => {
                self.open_chat_over(now_ns);
                if is_heard {
                    self.press_chat(now_ns);
                }
            }
        }
    }

    /// Takes the photo taken for the robot: where the photo viewfinder waits for it, the conversation opens over it with the photo for the next message; true if the screen changed.
    #[must_use]
    pub fn take_shot(&mut self, shot: Shot, now_ns: u64) -> bool {
        let is_awaited =
            self.screen == Some(Screen::Finder(Kind::Photo)) && core::mem::take(&mut self.finder.is_looking);
        if is_awaited {
            self.open_chat_over(now_ns);
            self.chat.bids.push(Bid::Job(Chore::Show(shot)));
        }
        is_awaited
    }

    // The press that turns the screen on, out of a sleep where `has_slept`, else out of the dark: it clicks nothing, and held as long as a hold to talk takes it talks, out of a sleep to the robot from the screen that woke, out of the dark as a hold on the screen that shows does.
    pub(super) fn press_waking(&mut self, has_slept: bool, now_ns: u64) {
        let is_shut = self.safe.is_shut || self.keying.window.is_open;
        if !self.is_button_down || is_shut || matches!(self.screen, Some(Screen::Key | Screen::Safe)) {
            return;
        }
        let has_own_voice = matches!(self.entered_screen(), Some(Screen::Pokedex | Screen::Web | Screen::Conversation));
        if has_slept && has_own_voice {
            self.leave_screen();
            self.go_back(now_ns);
        }
        let voice = if has_slept { Some(Screen::Chat) } else { self.talk_screen() };
        if let Some(voice) = voice {
            self.pressed_on = Some(self.screen);
            self.press_home(voice, now_ns);
        }
    }

    pub(crate) fn tick_home(&mut self, now_ns: u64) {
        let Some((start_ns, screen)) = self.talk_start else {
            return;
        };
        if self.pressed_on != Some(self.screen) {
            return self.let_go(now_ns);
        }
        if now_ns.saturating_sub(start_ns) >= HOLD_TO_TALK_NS {
            self.hold_home(screen, now_ns);
        }
    }

    // Goes back from a conversation a hold or a photo for the robot opened to what it opened over, the rail where it stood; false where none did.
    pub(super) fn return_from_chat(&mut self, now_ns: u64) -> bool {
        let (opened_from, rail_from) = (self.chat.opened_from.take(), self.chat.rail_from.take());
        if !self.does_chat_show() || opened_from.is_none() && rail_from.is_none() {
            return false;
        }
        self.depart(BACKWARD);
        self.screen = opened_from;
        if let Some(stand) = rail_from {
            self.return_rail(stand, now_ns);
        }
        if let Some(list) = self.visible_list() {
            self.deal_lines(list, |_| true);
        }
        true
    }

    pub(crate) fn let_go_home(&mut self) {
        let Some((_, screen)) = self.talk_start.take().filter(|(_, screen)| self.does_hear(*screen)) else {
            return;
        };
        match (screen, RequestList::of(screen)) {
            (_, Some(list)) => self.drop_request_press(list),
            (Screen::Pokedex, None) => self.pokedex.bids.push(Bid::Drop),
            (Screen::Web, None) => self.web.bids.push(Bid::Drop),
            (Screen::Conversation, None) => self.signal.bids.push(Bid::Drop),
            (_, None) => self.chat.bids.push(Bid::Drop),
        }
    }

    pub(super) fn press_chat(&mut self, now_ns: u64) {
        if self.chat.is_muted {
            self.hold_start = Some(now_ns);
            return;
        }
        self.chat.voice.is_talking = true;
        self.chat.bids.push(Bid::Listen);
    }

    pub(super) fn release_chat(&mut self) {
        if core::mem::take(&mut self.chat.voice.is_talking) {
            self.chat.bids.push(Bid::Send);
            return;
        }
        if self.chat.is_muted && self.hold_start.take().is_some() && self.chat.keys.ask_done() {
            self.send_chat();
        }
    }

    pub(super) fn send_chat(&mut self) {
        if let Some(text) = take_typed(&mut self.chat.keys) {
            (self.chat.room.at, self.chat.is_below) = (0, false);
            self.chat.bids.push(Bid::Text(text));
        }
    }

    pub(super) fn toggle_mute(&mut self, now_ns: u64) {
        self.chat.is_muted = !self.chat.is_muted;
        self.lay_chat_live();
        raise(&mut self.chat.keys, self.chat.is_muted, MAX_TEXT_CHARS);
        self.chat.room.scroll = Edge::still(self.chat_aim());
        self.hush_robot(self.chat.is_muted, now_ns);
        self.chat.bids.push(Bid::Job(Chore::Mute(self.chat.is_muted)));
    }

    /// True while a search's sound plays.
    pub fn does_search_sound(&self) -> bool {
        self.settings.sounds > volume::QUIETEST
            && self.is_awake()
            && self.does_chat_show()
            && !self.chat.is_muted
            && self.chat.voice.phase == Phase::Searching
    }

    pub(in crate::ui) fn leave_chat(&mut self) {
        if self.does_chat_show() {
            self.chat.bids.push(Bid::Job(Chore::Hush));
            self.chat.keys.release();
            self.let_go_chat();
        }
    }

    pub(crate) fn let_go_chat(&mut self) {
        if core::mem::take(&mut self.chat.voice.is_talking) {
            self.chat.bids.push(Bid::Send);
        }
    }

    pub(super) fn is_chat_unwatched(&self) -> bool {
        self.does_chat_show() && !self.chat.is_muted && self.chat.voice.phase.is_working()
    }

    pub(crate) fn land_chat(&mut self) {
        self.chat.keys.land();
        (self.chat.room.scroll, self.chat.band) = (Edge::still(self.chat_aim()), Edge::still(self.chat_band_aim()));
        self.chat.room.plate.land();
    }

    pub(super) fn view_chat<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        if !self.chat.voice.write_word(now_ns, word) {
            word.push_str(if self.chat.is_muted { MUTED } else { HOLD_TO_TALK });
        }
        let word: &'a str = word;
        let state = if self.chat.is_muted { MUTED } else { VOICE };
        View::Chat {
            name: &self.chat.name,
            words: BarWords { status: Some(word), state: Some(state), ..BarWords::default() },
            caption: if self.chat.voice.phase == Phase::Idle { &self.chat.name } else { word },
            hint: HOLD_THE_BUTTON,
        }
    }
}
