use super::action::Action;
use super::bar::BarWords;
use super::finder::turned;
use super::memos::cut_short;
use super::signal::{PERSON_ICON, contact_item, name_of};
use super::{BACKWARD, FORWARD, Screen, Stopwatch, View, WindowContent, seconds_since};
use crate::base::time::offset::to_local;
use crate::base::time::{MILLIS_PER_SECOND, Moment, write_duration};
use crate::domain::calls::{Ending, Member, Order, Phase, View as CallView};
use crate::domain::camera::Viewfinder;
use crate::domain::chat::tools::Destination;
use crate::domain::gallery;
use crate::domain::scene::{CALLS, MUTED};
use crate::domain::signal::list::{CallRow, Contactable};
use crate::domain::uses::Use;
use crate::gfx::Picture;
use crate::gfx::key::Ids;
use crate::hw::camera::swivel::Facing;
use crate::signal::address::Peer;
use crate::signal::call::Kind;
use crate::signal::command::Command;
use crate::signal::device::Status;
use crate::ui::grid::Grid;
use crate::ui::item::{Aside, Item};
use crate::ui::robot::Reaction;
use crate::ui::view::lines::List;
use crate::ui::view::window::Window;
use crate::ui::words::{CONNECTING, ENDED, ENDING, NEW_MARK, NO_CONTACTS, RINGING, cause_word};
use crate::ui::{NOTE_NS, Ui};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const NEW_CALL: &str = "NEW CALL";
const TO_ANYONE: &str = "TO A CONTACT OR A GROUP";
const NO_CALLS: &str = "NO CALLS YET";
const CALL: &str = "CALL";
const MISSED: &str = "MISSED";
const NONE_MISSED: &str = "NONE MISSED";
const INCOMING: &str = "INCOMING";
const OUTGOING: &str = "OUTGOING";
const AUDIO: &str = "AUDIO";
const VIDEO: &str = "VIDEO";
const GROUP: &str = "GROUP";
const ANSWERED: &str = "ANSWERED";
const DECLINED: &str = "DECLINED";
const STARTED: &str = "STARTED";
const RANG: &str = "RANG";
const JOINED: &str = "JOINED";
const SOUND_ALONE: &str = "SOUND ALONE";
const SOUND_AND_PICTURE: &str = "SOUND AND PICTURE";
const CALLING: &str = "CALLING";
const TALKING: &str = "TALKING";
const RECONNECTING: &str = "RECONNECTING";
const YOU: &str = "YOU";
const MICROPHONE_ON: &str = "MICROPHONE ON";
const MICROPHONE_OFF: &str = "MICROPHONE OFF";
const SPEAKING: &str = "SPEAKING";
const IN_THE_CALL: &str = "IN THE CALL";
const ANSWER: &str = "ANSWER";
const DECLINE: &str = "DECLINE";
const THE_CALL: &str = "THE CALL";
const THE_AUDIO_CALL: &str = "THE AUDIO CALL";
const THE_VIDEO_CALL: &str = "THE VIDEO CALL";
pub(super) const CALL_ICON: &str = icon!("call");
const NEW_ICON: &str = icon!("add_call");
const VIDEO_ICON: &str = icon!("videocam");
const END_ICON: &str = icon!("call_end");
const MADE_ICON: &str = icon!("call_made");
const RECEIVED_ICON: &str = icon!("call_received");
const MISSED_ICON: &str = icon!("call_missed");
const UNANSWERED_ICON: &str = icon!("call_missed_outgoing");
const MIC_ICON: &str = icon!("mic");
const MUTED_ICON: &str = icon!("mic_off");
pub(super) const ICONS: [&str; 10] = [
    CALL_ICON,
    NEW_ICON,
    VIDEO_ICON,
    END_ICON,
    MADE_ICON,
    RECEIVED_ICON,
    MISSED_ICON,
    UNANSWERED_ICON,
    MIC_ICON,
    MUTED_ICON,
];
// The log's first line opens whom a new call may go to; the calls follow it.
const FIRST_CALL: usize = 1;
// A call's first line is the owner's; the others in a group's call follow it.
const FIRST_MEMBER: usize = 1;
// The choice's lines: a name, the word under it, its icon, and whether the call sends this side's picture.
const CHOICES: [(&str, &str, &str, bool); 2] =
    [(AUDIO, SOUND_ALONE, CALL_ICON, false), (VIDEO, SOUND_AND_PICTURE, VIDEO_ICON, true)];
// The lines of the window of a call that rings.
const ANSWER_LINE: usize = 0;
const DECLINE_LINE: usize = 1;
const ANSWERS: usize = 2;

const fn stands(phase: Phase) -> bool {
    !matches!(phase, Phase::Ended(_))
}

// Why a call is over, as its screen tells it.
const fn ending_word(ending: Ending) -> &'static str {
    match ending {
        Ending::HungUp => ENDED,
        Ending::FarHungUp => "HUNG UP",
        Ending::Declined => DECLINED,
        Ending::Busy => "BUSY",
        Ending::Unanswered => "NOT ANSWERED",
        Ending::Elsewhere => "ON ANOTHER DEVICE",
        Ending::Unpermitted => "NOT ACCEPTED YET",
        Ending::Full => "CALL IS FULL",
        Ending::Failed(cause) => cause_word(cause),
    }
}

// How a call of the log came out, none for one under way unanswered.
const fn outcome_word(status: Status) -> Option<&'static str> {
    Some(match status {
        Status::Accepted => ANSWERED,
        Status::Missed => MISSED,
        Status::Declined => DECLINED,
        Status::Generic => STARTED,
        Status::OutgoingRing => RANG,
        Status::Ringing => RINGING,
        Status::Joined => JOINED,
        Status::Pending | Status::Deleted => return None,
    })
}

// A call of the log as its line tells it: which way it went and of what kind, how it came out, and when by the clock on screen, the day left out where it is today's.
fn to_call_word(row: &CallRow, offset: i16, today: Option<(u32, u8, u8)>) -> String {
    let way = if row.is_outgoing { OUTGOING } else { INCOMING };
    let kind = match row.kind {
        Kind::Audio => AUDIO,
        Kind::Video => VIDEO,
        Kind::Group => GROUP,
    };
    let mut word = format!("{way} {kind}, ");
    if let Some(outcome) = outcome_word(row.status) {
        word.push_str(outcome);
        word.push_str(", ");
    }
    let time = Moment::from_seconds(to_local(row.at / MILLIS_PER_SECOND, offset));
    if today != Some((time.year, time.month, time.day)) {
        let _ = write!(word, "{} {} ", time.day, cut_short(time.month_name()));
    }
    word.push_str(&time.to_time_text());
    word
}

const fn call_icon(row: &CallRow) -> &'static str {
    match (row.is_outgoing, row.status) {
        (false, Status::Missed) => MISSED_ICON,
        (true, Status::Missed | Status::Declined) => UNANSWERED_ICON,
        (false, _) => RECEIVED_ICON,
        (true, _) => MADE_ICON,
    }
}

const fn member_word(member: &Member) -> &'static str {
    match (member.is_speaking, member.is_muted) {
        (true, _) => SPEAKING,
        (false, true) => MUTED,
        (false, false) => IN_THE_CALL,
    }
}

pub(crate) struct Calls {
    // The log, the newest first, and whom a call may go to, as Signal's thread last told them.
    log: Vec<CallRow>,
    callees: Vec<Contactable>,
    // The list shows whom a new call may go to, in place of the log.
    pub(super) is_choosing: bool,
    // Whom the choice between a call with sound alone and one with this side's picture is for, by the name its line showed.
    chosen: Option<(Peer, Option<String>)>,
    choice_title: String,
    // The time of the newest call not seen yet that Signal was told the log showed.
    seen_at: Option<u64>,
    pub(crate) grid: Grid,
    pub(crate) choice: Grid,
    pub(crate) lines: Grid,
    pub(crate) answers: Grid,
    pub(crate) window: Window,
    // How the call stands, as the call layer last told it or as the choice ordered it.
    view: Option<CallView>,
    call_title: String,
    orders: Vec<Order>,
    pub(crate) talking: Stopwatch,
    // The screen the call's screen goes back to.
    beneath: Option<Screen>,
    // When the call ended, which its screen tells for as long as a note shows.
    ended_ns: Option<u64>,
    // The owner answered the ring or sent it away; the owner ended the call; its screen or window has shown.
    is_answered: bool,
    is_ending: bool,
    has_shown: bool,
    facing: Facing,
    far: Picture,
    pub(crate) far_id: u64,
    own: Vec<u32>,
    pub(crate) own_id: u64,
}

impl Calls {
    pub(crate) fn new(ids: &Ids) -> Calls {
        Calls {
            log: Vec::new(),
            callees: Vec::new(),
            is_choosing: false,
            chosen: None,
            choice_title: String::new(),
            seen_at: None,
            grid: Grid::empty(),
            choice: Grid::empty(),
            lines: Grid::empty(),
            answers: Grid::empty(),
            window: Window::new(CALLS),
            view: None,
            call_title: String::new(),
            orders: Vec::new(),
            talking: Stopwatch::default(),
            beneath: None,
            ended_ns: None,
            is_answered: false,
            is_ending: false,
            has_shown: false,
            facing: Facing::Front,
            far: Picture::default(),
            far_id: ids.next(),
            own: Vec::new(),
            own_id: ids.next(),
        }
    }

    // A call begins: nothing of the one before holds, and this side's camera looks at the owner.
    const fn begin(&mut self) {
        (self.ended_ns, self.is_answered, self.is_ending, self.has_shown) = (None, false, false, false);
        self.facing = Facing::Front;
    }
}

impl Ui {
    /// Takes the call log, the newest first, and whom a call may go to, each where it changed; true if the screen changed.
    #[must_use]
    pub fn take_call_log(&mut self, log: Option<Vec<CallRow>>, callees: Option<Vec<Contactable>>) -> bool {
        let does_list_show = self.screen == Some(Screen::Calls);
        let (is_log_new, are_callees_new) = (log.is_some(), callees.is_some());
        if let Some(log) = log {
            self.calls.log = log;
        }
        if let Some(callees) = callees {
            self.calls.callees = callees;
        }
        let is_shown = does_list_show && if self.calls.is_choosing { are_callees_new } else { is_log_new };
        if is_shown {
            self.lay_calls();
            self.see_calls();
        }
        let is_line = is_log_new && self.redeal_lines(&[Use::Open(Destination::Calls)]);
        is_shown || is_line
    }

    /// Takes how the call stands, none where none does; whether the screen changed, and the wake a call that rings asks of a sleeping device.
    #[must_use]
    pub fn set_call(&mut self, view: Option<CallView>, now_ns: u64) -> (bool, Option<Action>) {
        if view == self.calls.view {
            return (false, None);
        }
        let (before, after) = (self.call_phase(), view.as_ref().map(|view| view.phase));
        if after.is_some_and(stands) && !before.is_some_and(stands) {
            self.calls.begin();
        }
        match view {
            Some(view) => self.take_call_view(view),
            None => self.calls.view = None,
        }
        if let Some(Phase::Ended(ending)) = after.filter(|_| before.is_some_and(stands)) {
            self.calls.ended_ns = Some(now_ns);
            if matches!(ending, Ending::Failed(_)) {
                self.react(Reaction::Slump, now_ns);
            }
        }
        let (was_ringing, is_ringing) = (before == Some(Phase::Incoming), after == Some(Phase::Incoming));
        let is_rung = is_ringing && !was_ringing && self.does_call_ring();
        if is_rung {
            self.is_dark = false;
            self.show_incoming(now_ns);
        }
        if was_ringing && !is_ringing && self.screen == Some(Screen::Incoming) {
            self.calls.window.is_closing = true;
        }
        if after.is_none() && self.screen == Some(Screen::Call) {
            self.leave_call_screen();
        }
        if self.screen == Some(Screen::Call) && !self.is_video_call() {
            self.lay_call_lines();
        }
        let action = (is_rung && !self.is_awake()).then_some(Action::Wake);
        (matches!(self.screen, Some(Screen::Call | Screen::Incoming)), action)
    }

    fn take_call_view(&mut self, view: CallView) {
        self.calls.call_title = name_of(view.name.as_deref()).to_uppercase();
        let since = view.since.filter(|_| view.phase == Phase::Talking);
        self.calls.talking.start_ns = since.and_then(|since| u64::try_from(since.as_nanos()).ok());
        self.calls.view = Some(view);
    }

    /// The orders the calls' screens made since this was last taken.
    pub fn take_call_orders(&mut self) -> Vec<Order> {
        core::mem::take(&mut self.calls.orders)
    }

    /// True while Signal's socket is held for the calls: their list or its choice shows on a device awake, or a call stands, on its screen or, as `is_standing` tells, in the call layer before its screen has heard of it.
    pub fn are_calls_wanted(&self, is_standing: bool) -> bool {
        let is_listed = self.is_awake() && matches!(self.screen, Some(Screen::Calls | Screen::CallChoice));
        is_listed || self.does_call_stand() || is_standing
    }

    /// The viewfinder that takes this side's picture while awake and no viewfinder shows on the screen: wanted by a call that sends it, or held by the console.
    pub fn call_camera(&self) -> Option<Viewfinder> {
        let is_sent = self.calls.view.as_ref().is_some_and(|view| view.is_video_on) && self.is_call_talking();
        let is_free = self.is_awake() && self.visible_camera().is_none();
        ((is_sent || self.is_camera_held) && is_free).then(|| self.call_viewfinder())
    }

    // The viewfinder of this side's picture in a call: the way its camera looks, and the width the screen shows the picture at.
    pub(in crate::ui) fn call_viewfinder(&self) -> Viewfinder {
        let width = self.own_width();
        Viewfinder {
            kind: gallery::Kind::Photo,
            facing: self.calls.facing,
            is_scanning: false,
            is_calling: true,
            size: (width, width),
        }
    }

    /// The size the far side's picture is fitted to: the room above this side's or the one left of it, as the shape last shown stands larger.
    pub fn far_picture_room(&self) -> (usize, usize) {
        let far = self.far_area();
        (far.w.max(0) as usize, far.h.max(0) as usize)
    }

    /// The far side's picture, into which the call swaps its newest one.
    pub const fn far_picture_mut(&mut self) -> &mut Picture {
        &mut self.calls.far
    }

    /// Notes a new picture of the far side's; true if the screen changed.
    #[must_use]
    pub fn note_far_frame(&mut self) -> bool {
        self.calls.far_id = self.ids.next();
        self.does_video_show()
    }

    /// This side's picture, into which the camera swaps its newest one.
    pub const fn own_picture_mut(&mut self) -> &mut Vec<u32> {
        &mut self.calls.own
    }

    /// Notes a new picture of this side's; true if the screen changed.
    #[must_use]
    pub fn note_own_frame(&mut self) -> bool {
        self.calls.own_id = self.ids.next();
        self.does_video_show()
    }

    fn does_video_show(&self) -> bool {
        self.screen == Some(Screen::Call) && self.is_video_call()
    }

    fn call_phase(&self) -> Option<Phase> {
        self.calls.view.as_ref().map(|view| view.phase)
    }

    // True while a call stands: it rings, is on its way or is talked in.
    pub(super) fn does_call_stand(&self) -> bool {
        self.call_phase().is_some_and(stands)
    }

    // True while a call that stands is no longer only ringing here.
    pub(super) fn is_call_talking(&self) -> bool {
        self.does_call_stand() && self.call_phase() != Some(Phase::Incoming)
    }

    pub(in crate::ui) fn is_video_call(&self) -> bool {
        self.calls.view.as_ref().is_some_and(|view| view.is_video)
    }

    // True while the call's screen hears the owner: an audio call talked in, its microphone on.
    pub(in crate::ui) fn is_hearing_call(&self) -> bool {
        let is_heard = self.calls.view.as_ref().is_some_and(|view| view.phase == Phase::Talking && !view.is_muted);
        self.screen == Some(Screen::Call) && is_heard && !self.is_video_call()
    }

    // An audio call needs nobody watching; a video call is watched.
    pub(super) fn is_call_unwatched(&self) -> bool {
        self.is_call_talking() && !self.is_video_call()
    }

    // True while a call rings here and the owner has neither answered it nor sent it away; not while the key's window may not be left, which no ring's window rises over: the call then rings at the owner's other devices alone, as sending it away here would end it there too.
    pub(super) fn does_call_ring(&self) -> bool {
        self.call_phase() == Some(Phase::Incoming) && !self.calls.is_answered && !self.is_key_held()
    }

    // True while the call's screen or the window of its ring shows, or a window that rose over its screen.
    fn does_call_show(&self) -> bool {
        matches!(self.screen, Some(Screen::Call | Screen::Incoming))
            || self.open_window().is_some_and(|window| window.beneath == Some(Screen::Call))
    }

    pub(in crate::ui) fn far_picture(&self) -> Option<&Picture> {
        let does_come = self.calls.view.as_ref().is_some_and(|view| view.has_far_video);
        Some(&self.calls.far).filter(|picture| does_come && picture.width > 0)
    }

    pub(in crate::ui) fn own_picture(&self) -> &[u32] {
        let is_sent = self.calls.view.as_ref().is_some_and(|view| view.is_video_on);
        if is_sent { &self.calls.own } else { &[] }
    }

    pub(in crate::ui) fn call_title(&self) -> &str {
        &self.calls.call_title
    }

    pub(super) fn write_calls_word(&self, text: &mut String) {
        let unseen = self.calls.log.iter().filter(|row| !row.is_seen).count();
        let _ = match (unseen, self.calls.log.is_empty()) {
            (0, true) => text.write_str(NO_CALLS),
            (0, false) => text.write_str(NONE_MISSED),
            (unseen, _) => write!(text, "{unseen} {MISSED}"),
        };
    }

    // The log shows, the newest call chosen where there is one, else the line that opens whom a call may go to.
    pub(super) fn open_calls(&mut self) {
        self.calls_list_at(None);
    }

    pub(super) fn calls_list_at(&mut self, line: Option<usize>) {
        self.calls.is_choosing = false;
        self.screen = Some(Screen::Calls);
        let count = self.calls_count();
        self.show_lines(List::Calls, count, Some(line.unwrap_or(if count > FIRST_CALL { FIRST_CALL } else { 0 })));
        self.see_calls();
    }

    fn calls_count(&self) -> usize {
        if self.calls.is_choosing { self.calls.callees.len() } else { FIRST_CALL + self.calls.log.len() }
    }

    pub(super) fn lay_calls(&mut self) {
        self.lay_lines(List::Calls, self.calls_count());
    }

    // The log shows: Signal is told so once for each newer call not seen yet, all of them seen from then on.
    fn see_calls(&mut self) {
        let newest = self.calls.log.iter().filter(|row| !row.is_seen).map(|row| row.at).max();
        let is_new = newest.is_some_and(|at| self.calls.seen_at.is_none_or(|seen| at > seen));
        if is_new && self.is_awake() && self.screen == Some(Screen::Calls) && !self.calls.is_choosing {
            self.calls.seen_at = newest;
            self.ask_signal(Command::Seen);
        }
    }

    pub(in crate::ui) fn calls_item(&self, index: usize) -> Option<Item> {
        if self.calls.is_choosing {
            return self.calls.callees.get(index).map(contact_item);
        }
        let Some(at) = index.checked_sub(FIRST_CALL) else {
            return Some(Item::lead(NEW_CALL, TO_ANYONE).with_icon(NEW_ICON));
        };
        let row = self.calls.log.get(at)?;
        let mut word = String::from(if row.is_seen { "" } else { NEW_MARK });
        word.push_str(&to_call_word(row, self.settings.offset, self.today()));
        Some(Item::lead(name_of(row.name.as_deref()), word).with_icon(call_icon(row)))
    }

    // A click on the log's first line lists whom a call may go to; one on a call or on whom it may go to asks with what.
    pub(super) fn pick_call(&mut self) {
        let at = self.calls.grid.selection;
        let chosen = if self.calls.is_choosing {
            self.calls.callees.get(at).map(|callee| (callee.peer, Some(callee.name.clone())))
        } else {
            match at.checked_sub(FIRST_CALL) {
                None => return self.choose_callee(),
                Some(at) => self.calls.log.get(at).map(|row| (row.peer, row.name.clone())),
            }
        };
        if let Some((peer, name)) = chosen {
            self.open_call_choice(peer, name);
        }
    }

    fn choose_callee(&mut self) {
        self.calls.is_choosing = true;
        let count = self.calls_count();
        self.show_lines(List::Calls, count, None);
    }

    fn open_call_choice(&mut self, peer: Peer, name: Option<String>) {
        self.depart(FORWARD);
        self.calls.choice_title = name_of(name.as_deref()).to_uppercase();
        self.calls.chosen = Some((peer, name));
        self.show_lines(List::CallChoice, CHOICES.len(), None);
        self.screen = Some(Screen::CallChoice);
    }

    pub(in crate::ui) fn choice_item(&self, index: usize) -> Option<Item> {
        let &(name, word, icon, _) = CHOICES.get(index)?;
        Some(Item::lead(name, word).with_icon(icon))
    }

    // A click on the choice orders the call, whose screen shows it on its way at once.
    pub(super) fn pick_call_choice(&mut self) {
        let (Some((peer, name)), Some(choice)) = (self.calls.chosen.clone(), CHOICES.get(self.calls.choice.selection))
        else {
            return;
        };
        let is_video = choice.3;
        self.calls.orders.push(Order::Call { peer, name: name.clone(), is_video });
        let view = CallView {
            peer,
            name,
            is_outgoing: true,
            is_video,
            phase: Phase::Calling,
            since: None,
            is_muted: false,
            is_video_on: is_video,
            has_far_video: false,
            members: Vec::new(),
        };
        self.depart(FORWARD);
        self.calls.begin();
        (self.calls.has_shown, self.calls.beneath) = (true, Some(Screen::Calls));
        self.take_call_view(view);
        (self.screen, self.note) = (Some(Screen::Call), None);
        self.show_call_lines();
    }

    fn call_line_count(&self) -> usize {
        self.calls.view.as_ref().map_or(0, |view| FIRST_MEMBER + view.members.len())
    }

    fn show_call_lines(&mut self) {
        let count = self.call_line_count();
        self.show_lines(List::Call, count, Some(0));
    }

    fn lay_call_lines(&mut self) {
        let count = self.call_line_count();
        if self.calls.lines.items != Some(count) {
            self.calls.lines.set_items(count);
        }
        self.deal_lines(List::Call, |_| true);
    }

    pub(in crate::ui) fn call_item(&self, index: usize) -> Option<Item> {
        let view = self.calls.view.as_ref()?;
        let Some(at) = index.checked_sub(FIRST_MEMBER) else {
            let (word, icon) = match (stands(view.phase), view.is_muted) {
                (false, _) => (MICROPHONE_OFF, MUTED_ICON),
                (true, true) => (MUTED, MUTED_ICON),
                (true, false) => (MICROPHONE_ON, MIC_ICON),
            };
            let item = Item::new(YOU, word).with_icon(icon);
            return Some(if self.is_hearing_call() { item.with_aside(Aside::Meter) } else { item });
        };
        let member = view.members.get(at)?;
        Some(Item::new(name_of(member.name.as_deref()), member_word(member)).with_icon(PERSON_ICON))
    }

    // A click ends the call; once it is over, the screen goes back at once.
    pub(super) fn click_call(&mut self) {
        if self.does_call_stand() {
            self.end_call();
        } else {
            self.leave_call_screen();
        }
    }

    // A swipe back ends the call and goes back.
    pub(super) fn leave_call(&mut self) {
        self.end_call();
        self.leave_call_screen();
    }

    fn end_call(&mut self) {
        if self.does_call_stand() && !self.calls.is_ending {
            self.calls.is_ending = true;
            self.calls.orders.push(Order::End);
        }
    }

    fn leave_call_screen(&mut self) {
        self.depart(BACKWARD);
        (self.screen, self.calls.ended_ns, self.note) = (self.calls.beneath.take(), None, None);
        if self.screen == Some(Screen::Calls) {
            self.lay_calls();
            self.see_calls();
        } else if let Some(list) = self.visible_list() {
            self.deal_lines(list, |_| true);
        }
    }

    // A touch on the robot has the microphone heard no more, or again.
    pub(super) fn mute_call(&mut self, now_ns: u64) {
        let Some(view) = self.calls.view.as_mut() else {
            return;
        };
        view.is_muted = !view.is_muted;
        let is_muted = view.is_muted;
        self.calls.orders.push(Order::Mute(is_muted));
        self.hush_robot(is_muted, now_ns);
        self.lay_call_lines();
    }

    pub(super) fn turn_call_camera(&mut self, detents: i32) {
        self.calls.facing = turned(self.calls.facing, detents);
    }

    // Raises the window of a call that rings over what shows, once neither the passphrase's nor the key's window stands, the bell's giving way to it.
    pub(in crate::ui) fn show_incoming(&mut self, now_ns: u64) {
        let is_barred = self.safe.is_shut || self.safe.window.is_open || self.keying.window.is_open;
        if !self.does_call_ring() || self.calls.has_shown || is_barred {
            return;
        }
        self.leave_screen();
        if self.pressed_on.is_some() {
            self.let_go(now_ns);
        }
        let beneath = if self.screen == Some(Screen::Ring) {
            let beneath = self.bells.window.beneath;
            self.lower_ring();
            beneath
        } else {
            self.screen
        };
        // A call's screen beneath is that of a call already over, whose place the one that rings takes: its way back is that call's.
        let beneath = if beneath == Some(Screen::Call) { self.calls.beneath.take() } else { beneath };
        self.passage = None;
        self.calls.has_shown = true;
        self.calls.window.rise_over(beneath, self.ruler.height);
        self.screen = Some(Screen::Incoming);
        self.lay_answers();
    }

    pub(in crate::ui) fn lay_answers(&mut self) {
        let grid = &mut self.calls.answers;
        grid.begin();
        grid.set_items_at(ANSWERS, ANSWER_LINE);
        grid.land();
        self.deal_lines(List::Incoming, |_| true);
    }

    pub(in crate::ui) fn answer_item(&self, index: usize) -> Option<Item> {
        let is_video = self.is_video_call();
        match index {
            ANSWER_LINE => {
                Some(Item::new(ANSWER, if is_video { THE_VIDEO_CALL } else { THE_AUDIO_CALL }).with_icon(CALL_ICON))
            }
            DECLINE_LINE => Some(Item::new(DECLINE, THE_CALL).with_icon(END_ICON)),
            _ => None,
        }
    }

    // A click on the window answers the call, which its screen shows as the window falls onto it, or sends it away.
    pub(super) fn pick_answer(&mut self) {
        if self.calls.window.is_closing {
            return;
        }
        if self.calls.answers.selection == DECLINE_LINE {
            return self.decline_call();
        }
        self.calls.answers.press(false);
        self.calls.is_answered = true;
        self.calls.orders.push(Order::Accept);
        (self.calls.beneath, self.calls.window.beneath) = (self.calls.window.beneath, Some(Screen::Call));
        (self.calls.window.is_closing, self.note) = (true, None);
        self.show_call_lines();
        self.calls.lines.land();
    }

    // Sends the call that rings away, by a click on its line or a swipe back, the window falling onto what it rose over.
    pub(super) fn decline_call(&mut self) {
        if self.calls.window.is_closing {
            return;
        }
        self.calls.answers.press(false);
        (self.calls.is_answered, self.calls.is_ending) = (true, true);
        self.calls.orders.push(Order::Decline);
        self.calls.window.is_closing = true;
    }

    pub(in crate::ui) fn incoming_gone(&mut self) {
        self.calls.window.is_closing = false;
        if let Some(list) = self.visible_list() {
            self.deal_lines(list, |_| true);
        }
        self.keep_asking();
    }

    // Raises the window of a call that rings where it waited, goes back from a call that is over once its ending has shown, ends a call whose screen was left, and lets go of the pictures once no call stands, this side's once its camera runs no more too.
    pub(in crate::ui) fn tick_call(&mut self, now_ns: u64) {
        self.show_incoming(now_ns);
        let is_over = self.calls.ended_ns.is_some_and(|ended_ns| now_ns >= ended_ns + NOTE_NS);
        if is_over && self.screen == Some(Screen::Call) {
            self.leave_call_screen();
        }
        self.keep_call();
        if !self.does_call_stand() && self.screen != Some(Screen::Call) {
            self.calls.far = Picture::default();
            // Kept while the camera runs, which draws its next picture into it.
            if self.call_camera().is_none() {
                self.calls.own = Vec::new();
            }
        }
    }

    // A call stands while its screen or window shows: left any other way than by a click or a swipe back, home or another screen taking its place, it is ended or sent away.
    fn keep_call(&mut self) {
        let Some(phase) = self.call_phase().filter(|phase| stands(*phase)) else {
            return;
        };
        if self.calls.is_ending || !self.calls.has_shown || self.does_call_show() {
            return;
        }
        let is_ringing = phase == Phase::Incoming && !self.calls.is_answered;
        (self.calls.is_answered, self.calls.is_ending) = (true, true);
        self.calls.orders.push(if is_ringing { Order::Decline } else { Order::End });
    }

    pub(in crate::ui) fn call_due(&self) -> Option<u64> {
        let is_shown = self.screen == Some(Screen::Call);
        let ended = self.calls.ended_ns.filter(|_| is_shown).map(|ended_ns| ended_ns + NOTE_NS);
        ended.or(self.calls.talking.due(is_shown))
    }

    pub(super) fn view_calls<'a>(&'a self, word: &'a mut String) -> View<'a> {
        let count = self.calls_count();
        let (name, place) = if self.calls.is_choosing {
            if count == 0 {
                word.push_str(NO_CONTACTS);
            }
            (NEW_CALL, (count > 0).then(|| (self.calls.grid.selection + 1, count)))
        } else {
            let (calls, at) = (self.calls.log.len(), self.calls.grid.selection.checked_sub(FIRST_CALL));
            let _ = match (calls, at) {
                (0, _) => word.write_str(NO_CALLS),
                (1, None) => write!(word, "1 {CALL}"),
                (_, None) => write!(word, "{calls} {CALLS}"),
                (_, Some(_)) => Ok(()),
            };
            (CALLS, at.map(|at| (at + 1, calls)))
        };
        let subject: &'a str = word;
        let words = BarWords {
            status: self.signal_status(),
            subject: Some(subject).filter(|subject| !subject.is_empty()),
            place,
            ..BarWords::default()
        };
        View::Lines { list: List::Calls, name, words }
    }

    pub(super) fn view_call_choice(&self) -> View<'_> {
        let words = BarWords { place: self.place_in(List::CallChoice), ..BarWords::default() };
        View::Lines { list: List::CallChoice, name: self.cut_title(&self.calls.choice_title), words }
    }

    fn write_call_status(&self, now_ns: u64, text: &mut String) {
        let Some(view) = self.calls.view.as_ref() else {
            return;
        };
        let word = match view.phase {
            Phase::Ended(ending) => ending_word(ending),
            _ if self.calls.is_ending => ENDING,
            Phase::Calling => CALLING,
            Phase::Ringing => RINGING,
            Phase::Incoming if self.calls.is_answered => CONNECTING,
            Phase::Incoming => RINGING,
            Phase::Connecting => CONNECTING,
            Phase::Reconnecting => RECONNECTING,
            Phase::Talking => {
                text.push_str(if view.is_muted { MUTED } else { TALKING });
                text.push(' ');
                write_duration(text, self.calls.talking.start_ns.map_or(0, |start_ns| seconds_since(start_ns, now_ns)));
                return;
            }
        };
        text.push_str(word);
    }

    pub(super) fn view_call<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        match &self.note {
            Some((note, _)) => word.push_str(note),
            None => self.write_call_status(now_ns, word),
        }
        let status: &'a str = word;
        let words = BarWords { status: Some(status).filter(|status| !status.is_empty()), ..BarWords::default() };
        View::Call { name: &self.calls.call_title, words, is_video: self.is_video_call() }
    }

    pub(super) fn view_incoming(&self) -> View<'_> {
        let words = BarWords { status: Some(RINGING), place: self.place_in(List::Incoming), ..BarWords::default() };
        View::Window { words, content: WindowContent::Lines(List::Incoming) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cause::Cause;
    use crate::signal::address::ServiceId;

    // Friday 20 November 2026, 14:03 UTC.
    const AT_MS: u64 = 1_795_183_380_000;
    const TODAY: Option<(u32, u8, u8)> = Some((2026, 11, 20));

    fn row(kind: Kind, is_outgoing: bool, status: Status) -> CallRow {
        CallRow {
            peer: Peer::Contact(ServiceId::Aci([1; 16])),
            name: None,
            kind,
            is_outgoing,
            status,
            at: AT_MS,
            is_seen: true,
        }
    }

    #[test]
    fn call_line_words() {
        assert_eq!(
            [
                to_call_word(&row(Kind::Audio, false, Status::Missed), 0, TODAY),
                to_call_word(&row(Kind::Video, true, Status::Accepted), 0, TODAY),
                to_call_word(&row(Kind::Group, false, Status::Generic), 0, TODAY),
                to_call_word(&row(Kind::Audio, true, Status::Pending), 0, TODAY),
            ],
            [
                "INCOMING AUDIO, MISSED, 14:03",
                "OUTGOING VIDEO, ANSWERED, 14:03",
                "INCOMING GROUP, STARTED, 14:03",
                "OUTGOING AUDIO, 14:03",
            ],
            "the way, the kind, the outcome where there is one, and today's time alone"
        );
        assert_eq!(
            [
                to_call_word(&row(Kind::Audio, true, Status::Declined), 0, Some((2026, 11, 21))),
                to_call_word(&row(Kind::Group, true, Status::OutgoingRing), 120, TODAY),
            ],
            ["OUTGOING AUDIO, DECLINED, 20 nov 14:03", "OUTGOING GROUP, RANG, 16:03"],
            "another day's day before its time, and the time by the clock's offset"
        );
    }

    #[test]
    fn call_line_icons() {
        assert_eq!(
            [
                call_icon(&row(Kind::Audio, false, Status::Missed)),
                call_icon(&row(Kind::Audio, false, Status::Declined)),
                call_icon(&row(Kind::Video, true, Status::Missed)),
                call_icon(&row(Kind::Video, true, Status::Declined)),
                call_icon(&row(Kind::Group, true, Status::Joined)),
            ],
            [MISSED_ICON, RECEIVED_ICON, UNANSWERED_ICON, UNANSWERED_ICON, MADE_ICON],
            "by the way it went, missed or not taken where it was"
        );
    }

    #[test]
    fn call_end_words() {
        let endings = [
            Ending::HungUp,
            Ending::FarHungUp,
            Ending::Declined,
            Ending::Busy,
            Ending::Unanswered,
            Ending::Elsewhere,
            Ending::Unpermitted,
            Ending::Full,
            Ending::Failed(Cause::NoNetwork),
        ];
        assert_eq!(
            endings.map(ending_word),
            [
                "ENDED",
                "HUNG UP",
                "DECLINED",
                "BUSY",
                "NOT ANSWERED",
                "ON ANOTHER DEVICE",
                "NOT ACCEPTED YET",
                "CALL IS FULL",
                "NO NETWORK"
            ],
            "each ending its word, a failure its cause's"
        );
        let member = |is_speaking, is_muted| Member { name: None, is_speaking, is_muted, has_video: false };
        assert_eq!(
            [member(true, false), member(false, true), member(false, false)].map(|member| member_word(&member)),
            ["SPEAKING", "MUTED", "IN THE CALL"],
            "who speaks, who is muted"
        );
    }
}
