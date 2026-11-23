use super::action::Action;
use super::bar::BarWords;
use super::typing::KeyboardId;
use super::voice::{Voice, VoiceId};
use super::{Round, Screen, View};
use crate::base::math::round;
use crate::base::text::has_words;
use crate::base::time::offset::to_local;
use crate::base::time::{MILLIS_PER_SECOND, Moment};
use crate::codec::qr::{self, Level, Symbol};
use crate::domain::cause::Cause;
use crate::domain::chat::MAX_TEXT_CHARS;
use crate::domain::chat::tools::Destination;
use crate::domain::gallery::Sending;
use crate::domain::paper::rows::{Row, wrap_lines};
use crate::domain::scene::{KEYS, SIGNAL};
use crate::domain::signal::drafts::Recording;
use crate::domain::signal::list::{Contactable, ConversationView, Summary, entry_words};
use crate::domain::signal::pictures::{Photo, standing_height};
use crate::domain::signal::state::{LinkStatus, LinkStep, Report, SignalView, ThreadStatus};
use crate::domain::uses::Use;
use crate::domain::voice::{Bid, Phase};
use crate::gfx::motion::Edge;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::attachment::Enclosure;
use crate::signal::command::{Command, Outgoing};
use crate::signal::content::Placeholder;
use crate::signal::conversation::{Deleter, Entry, Status};
use crate::signal::device::UNNAMED;
use crate::ui::field::{cells, cut};
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::keys::{Keyboard, raise, ready, take_typed};
use crate::ui::robot::Reaction;
use crate::ui::textroom::TextRoom;
use crate::ui::textroom::talk::{Line, LineKind, to_speaker};
use crate::ui::view::lines::List;
use crate::ui::view::settle::SCROLLING;
use crate::ui::words::{CONNECTING, HOLD_TO_TALK, NEW_MARK, NO_CONTACTS, READING, VOICE, cause_word};
use crate::ui::{FORWARD, HOLD_TO_TALK_NS, Ui};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const SCAN: &str = "SCAN WITH YOUR PHONE";
const WHERE: &str = "SIGNAL > SETTINGS > LINKED DEVICES";
const LINKING: &str = "LINKING";
const TYPE_A_MESSAGE: &str = "TYPE A MESSAGE";
const SENDING: &str = "SENDING";
const AUTO: &str = "AUTO";
const DRAFTED: &str = "CLICK TO SEND";
const NO_MESSAGES: &str = "NO MESSAGES YET";
const SEND_TO: &str = "SEND TO";
const TOO_LARGE: &str = "TOO LARGE TO SEND";
const NEW_MESSAGE: &str = "NEW MESSAGE";
const TO_A_CONTACT: &str = "TO A CONTACT";
const IN_YOUR_CHATS: &str = "IN YOUR CHATS";
const SENDING_NOTE: &str = "sending";
const NOT_SENT_NOTE: &str = "not sent";
const DELIVERED_NOTE: &str = "delivered";
const READ_NOTE: &str = "read";
const AUTO_NOTE: &str = "auto mode";
const EDITED_NOTE: &str = "edited";
const BY_ADMIN_NOTE: &str = "by an administrator";
const DRAFT_NOTE: &str = "draft, a click sends it";
const PLAYING_NOTE: &str = "playing";
const REPLYING: &str = "Reply to ";
const YOU: &str = "You";
const AFTER_NAME: &str = ": ";
const SECONDS: &str = "s";
const ALL_READ: &str = "ALL READ";
const UNREAD: &str = "UNREAD";
const LINK_A_PHONE: &str = "LINK A PHONE";
const CHAT: &str = "CHAT";
const CHATS: &str = "CHATS";
const ARCHIVED: &str = "ARCHIVED";
const ARCHIVED_ICON: &str = icon!("archive");
const NEW_ICON: &str = icon!("edit_square");
const SAFETY_CHANGED: &str = "SAFETY NUMBER CHANGED";
pub(super) const PERSON_ICON: &str = icon!("person");
const GROUP_ICON: &str = icon!("group");
pub(super) const ICONS: [&str; 4] = [PERSON_ICON, GROUP_ICON, ARCHIVED_ICON, NEW_ICON];
// The list's first line opens the contacts a new message may go to; the conversations follow it.
const FIRST_CONVERSATION: usize = 1;

// What stands at a place of the list: the line that opens the contacts, a conversation, the line of the archived ones, or, while the contacts show, a contact.
#[derive(Clone, Copy)]
enum Place<'a> {
    New,
    Conversation(&'a Summary),
    Archived(usize),
    Contact(&'a Contactable),
}

pub(crate) struct Signal {
    link_status: LinkStatus,
    link_step: LinkStep,
    is_connected: bool,
    failure: Option<Cause>,
    unread: usize,
    auto: Option<ServiceId>,
    code: Option<(String, Symbol)>,
    summaries: Vec<Summary>,
    pub(crate) grid: Grid,
    does_archive_show: bool,
    // The list shows the contacts a new message may go to, as the thread last told them, in place of the conversations.
    is_choosing: bool,
    contacts: Option<Vec<Contactable>>,
    peer: Option<Peer>,
    conversation: Option<ConversationView>,
    // True from a conversation's opening until the thread's view of it comes.
    is_awaited: bool,
    pub(super) is_typing: bool,
    pub(in crate::ui) keys: Keyboard,
    pub(crate) band: Edge,
    commands: Vec<Command>,
    pub(super) voice: Voice,
    reading_phase: Phase,
    round: Round,
    arrivals: Vec<String>,
    pub(super) bids: Vec<Bid>,
    draft: Option<String>,
    // The photo, the video or the voice note that waits under the draft for a click, with a voice note's seconds, and an item of a roll or a note that waits so for the conversation it goes to.
    carried: Option<(Enclosure, u64)>,
    pending: Option<(Enclosure, u64)>,
    draft_peer: Option<Peer>,
    // The photos the conversation that shows and its draft carry, each by its file as its picture was decoded to stand as wide as the room, and that width.
    pictures: Vec<Photo>,
    pictured: usize,
    // The voice notes the conversation that shows holds, each by its file and the foot of its message's rows; the one that plays by its file and the number of its playing; how many were played; the one the press that goes on stopped.
    notes: Vec<(String, i32)>,
    playing: Option<(String, u32)>,
    plays: u32,
    hushed: Option<String>,
    lines: Vec<Line>,
    pub(in crate::ui) height: i32,
    speaker: String,
    title: String,
    pub(in crate::ui) room: TextRoom<i32>,
}

impl Signal {
    pub(crate) fn new(keys: Keyboard) -> Signal {
        Signal {
            link_status: LinkStatus::Reading,
            link_step: LinkStep::Idle,
            is_connected: false,
            failure: None,
            unread: 0,
            auto: None,
            code: None,
            summaries: Vec::new(),
            grid: Grid::empty(),
            does_archive_show: false,
            is_choosing: false,
            contacts: None,
            peer: None,
            conversation: None,
            is_awaited: false,
            is_typing: false,
            keys,
            band: Edge::still(0.0),
            commands: Vec::new(),
            voice: Voice::new(),
            reading_phase: Phase::Idle,
            round: Round::default(),
            arrivals: Vec::new(),
            bids: Vec::new(),
            draft: None,
            carried: None,
            pending: None,
            draft_peer: None,
            pictures: Vec::new(),
            pictured: 0,
            notes: Vec::new(),
            playing: None,
            plays: 0,
            hushed: None,
            lines: Vec::new(),
            height: 0,
            speaker: String::new(),
            title: String::new(),
            room: TextRoom::default(),
        }
    }
}

const fn placeholder_word(placeholder: Placeholder) -> &'static str {
    match placeholder {
        Placeholder::Photo => "photo",
        Placeholder::Video => "video",
        Placeholder::VoiceNote => "voice note",
        Placeholder::File => "file",
        Placeholder::Sticker => "sticker",
        Placeholder::Contact => "contact",
        Placeholder::Poll => "poll",
        Placeholder::Unsupported => "needs a newer Signal",
    }
}

pub(super) fn name_of(name: Option<&str>) -> &str {
    name.filter(|name| has_words(name)).unwrap_or(UNNAMED)
}

const fn icon_of(peer: Peer) -> &'static str {
    match peer {
        Peer::Contact(_) => PERSON_ICON,
        Peer::Group(_) => GROUP_ICON,
    }
}

// The line of a contact or a group a message or a call may go to: its name over whether its conversation holds messages.
pub(super) fn contact_item(contact: &Contactable) -> Item {
    let word = if contact.has_talked { IN_YOUR_CHATS } else { NO_MESSAGES };
    Item::lead(name_of(Some(&contact.name)), word).with_icon(icon_of(contact.peer))
}

fn to_entry_text(entry: &Entry) -> String {
    if entry.deleter.is_some() || !entry.body.is_empty() {
        return entry_words(entry).replace('\n', " ");
    }
    entry.placeholders.first().map_or_else(String::new, |placeholder| String::from(placeholder_word(*placeholder)))
}

// A message's reactions as its side tells them: each emoji once, with how many gave it where more than one did.
fn to_reactions_note(entry: &Entry) -> String {
    let mut note = String::new();
    for (emoji, count) in entry.reaction_counts() {
        note.push_str(if note.is_empty() { "" } else { " " });
        note.push_str(emoji);
        if count > 1 {
            let _ = write!(note, "{count}");
        }
    }
    note
}

impl Ui {
    /// The Signal screen that shows.
    pub fn signal_view(&self) -> SignalView {
        match self.screen {
            Some(Screen::Link) => SignalView::Link,
            Some(Screen::Signal) if self.signal.is_choosing => SignalView::Contacts,
            Some(Screen::Signal) => SignalView::List,
            Some(Screen::Conversation) => self.signal.peer.map_or(SignalView::List, SignalView::Conversation),
            _ => SignalView::Nothing,
        }
    }

    /// The commands Signal's screens made since this was last taken.
    pub fn take_signal_commands(&mut self) -> Vec<Command> {
        core::mem::take(&mut self.signal.commands)
    }

    // Asks Signal's thread something for a screen of the calls'.
    pub(super) fn ask_signal(&mut self, command: Command) {
        self.signal.commands.push(command);
    }

    pub(in crate::ui) fn is_linked(&self) -> bool {
        self.signal.link_status == LinkStatus::Linked
    }

    /// The bids a conversation's button made since this was last taken.
    pub fn take_signal_bids(&mut self) -> Vec<Bid> {
        core::mem::take(&mut self.signal.bids)
    }

    fn signal_aloud(&self) -> Option<ServiceId> {
        let is_read = self.settings.is_signal_aloud && self.does_conversation_show() && !self.signal.is_typing;
        let contact = self.signal.peer.filter(|_| is_read)?.contact()?;
        (self.signal.auto != Some(contact)).then_some(contact)
    }

    fn is_arrival_heard(&self) -> bool {
        self.is_awake() && self.signal_aloud().is_some()
    }

    pub(super) fn signal_reading(&self) -> Option<(u32, &[String])> {
        let round = self.signal.round.current().filter(|_| self.is_arrival_heard())?;
        Some((round, self.signal.arrivals.as_slice()))
    }

    /// Ends a reading of arrivals nobody hears; true if the screen changed.
    #[must_use]
    pub fn end_unheard_reading(&mut self) -> bool {
        if self.is_arrival_heard() {
            return false;
        }
        core::mem::take(&mut self.signal.round.is_on) && self.set_arrival_phase(Phase::Idle)
    }

    /// Takes the phase of a reading of arrivals; true if the screen changed.
    #[must_use]
    pub fn set_signal_reading_phase(&mut self, round: u32, phase: Phase) -> bool {
        let signal = &mut self.signal;
        if signal.round.current() != Some(round) || !signal.round.note_phase(round, signal.reading_phase, phase) {
            return false;
        }
        self.set_arrival_phase(phase)
    }

    fn set_arrival_phase(&mut self, phase: Phase) -> bool {
        core::mem::replace(&mut self.signal.reading_phase, phase) != phase && self.does_conversation_show()
    }

    /// True while a conversation shows and the device is awake.
    pub fn are_drafts_wanted(&self) -> bool {
        self.is_awake() && self.does_conversation_show()
    }

    pub(crate) fn does_conversation_show(&self) -> bool {
        self.screen == Some(Screen::Conversation)
    }

    /// Takes the Signal thread's report, but for its reply to the robot; true if the screen changed.
    #[must_use]
    pub fn take_signal_report(&mut self, report: Report, now_ns: u64) -> bool {
        let does_show = matches!(self.screen, Some(Screen::Link | Screen::Signal | Screen::Conversation));
        let Report { summaries, conversation, contacts, arrivals, reply: _, ended, pictures, moved, status } = report;
        let ThreadStatus { link_status, link_step, is_connected, failure, unread, auto } = status;
        // A conversation that shows tells its own messages; asleep nothing shows, and what a fetch brought is told by the notice.
        let open = self.signal.peer.filter(|_| self.does_conversation_show() && self.is_awake());
        if arrivals.iter().any(|message| message.2 && Some(message.0) != open) {
            self.notice(true);
        }
        let read_peer = self.signal_aloud().map(Peer::Contact);
        let texts: Vec<String> =
            arrivals.into_iter().filter(|message| Some(message.0) == read_peer).map(|message| message.1).collect();
        let has_arrivals = !texts.is_empty();
        if has_arrivals {
            self.signal.round.begin();
            self.signal.arrivals = texts;
        }
        let has_new_phase = has_arrivals && self.set_arrival_phase(Phase::Connecting);
        let signal = &self.signal;
        let mut has_changed = (link_status, &link_step, is_connected, failure, unread, auto)
            != (signal.link_status, &signal.link_step, signal.is_connected, signal.failure, signal.unread, signal.auto);
        let is_line_new = (link_status, unread) != (signal.link_status, signal.unread);
        if let LinkStep::Code(address) = &link_step
            && signal.code.as_ref().is_none_or(|code| code.0 != *address)
        {
            self.signal.code = qr::make(address.as_bytes(), Level::Medium).map(|symbol| (address.clone(), symbol));
        }
        let previous = self.signal.link_status;
        (self.signal.link_status, self.signal.link_step, self.signal.is_connected) =
            (link_status, link_step, is_connected);
        (self.signal.failure, self.signal.unread, self.signal.auto) = (failure, unread, auto);
        let has_new_greeting = summaries.is_some() && self.does_greeting_show();
        if let Some(summaries) = summaries {
            self.signal.summaries = summaries;
            if self.screen == Some(Screen::Signal) {
                self.deal_signal();
            }
            has_changed = true;
        }
        if let Some(contacts) = contacts {
            self.signal.contacts = Some(contacts);
            if self.screen == Some(Screen::Signal) && self.signal.is_choosing {
                self.deal_signal();
                has_changed = true;
            }
        }
        let conversation = conversation.filter(|view| Some(view.peer) == self.signal.peer);
        let has_conversation = conversation.is_some();
        if let Some(conversation) = conversation {
            if conversation.entries.iter().any(|entry| !entry.is_owner && !entry.is_read)
                && self.does_conversation_show()
            {
                self.signal.commands.push(Command::Read(conversation.peer));
            }
            self.signal.conversation = Some(conversation);
        }
        self.follow_moved_photos(moved);
        if has_conversation {
            if core::mem::take(&mut self.signal.is_awaited) {
                self.lay_conversation_still();
            } else {
                self.lay_conversation();
            }
            has_changed = true;
        }
        if previous != LinkStatus::Linked && link_status == LinkStatus::Linked && self.screen == Some(Screen::Link) {
            self.open_signal_list();
            self.react(Reaction::Cheer, now_ns);
        } else if previous == LinkStatus::Linked && link_status == LinkStatus::Unlinked && does_show {
            self.leave_screen();
            self.screen = Some(Screen::Link);
        }
        // The calls' entry shows while the R1 is linked; their list and its choice give way once it is not.
        let is_revealed = (previous == LinkStatus::Linked) != (link_status == LinkStatus::Linked);
        if is_revealed {
            if link_status != LinkStatus::Linked && matches!(self.screen, Some(Screen::Calls | Screen::CallChoice)) {
                self.leave_screen();
                self.return_to_menu(now_ns);
            }
            self.reveal();
        }
        let is_line = is_line_new && self.redeal_lines(&[Use::Open(Destination::Signal)]);
        let is_sounded = ended.into_iter().fold(false, |is_changed, round| self.set_signal_played(round) | is_changed);
        let is_drawn = pictures.is_some_and(|drawn| self.set_signal_pictures(drawn));
        has_changed && does_show
            || is_line
            || has_new_greeting
            || has_new_phase
            || is_sounded
            || is_drawn
            || is_revealed
    }

    // What waited for a conversation waits no more.
    pub(super) fn open_signal(&mut self) {
        self.signal.pending = None;
        if self.signal.link_status == LinkStatus::Linked {
            self.open_signal_list();
        } else {
            self.screen = Some(Screen::Link);
        }
    }

    // The line after the one that opens the contacts is chosen where there is one, the first conversation or the archived ones' line, else that line.
    fn open_signal_list(&mut self) {
        self.signal_list_at(None);
    }

    fn signal_list_at(&mut self, line: Option<usize>) {
        (self.signal.is_choosing, self.signal.does_archive_show) = (false, false);
        self.screen = Some(Screen::Signal);
        let count = self.signal_count();
        let first = if count > FIRST_CONVERSATION { FIRST_CONVERSATION } else { 0 };
        self.show_lines(List::Signal, count, Some(line.unwrap_or(first)));
    }

    // The list shows the contacts, the first chosen; the thread is asked for them as they show.
    fn choose_contact(&mut self) {
        self.signal.is_choosing = true;
        let count = self.signal_count();
        self.show_lines(List::Signal, count, None);
    }

    // Leaves the contacts for the conversations, or whom a call may go to for the call log, at the line that opened them; true if they showed.
    pub(super) fn return_from_contacts(&mut self) -> bool {
        match self.screen {
            Some(Screen::Signal) if self.signal.is_choosing => self.signal_list_at(Some(0)),
            Some(Screen::Calls) if self.calls.is_choosing => self.calls_list_at(Some(0)),
            _ => return false,
        }
        true
    }

    fn summaries_of(&self, is_archived: bool) -> impl Iterator<Item = &Summary> {
        self.signal.summaries.iter().filter(move |summary| summary.is_archived == is_archived)
    }

    fn signal_count(&self) -> usize {
        if self.signal.is_choosing {
            return self.signal.contacts.as_ref().map_or(0, Vec::len);
        }
        let archived = self.summaries_of(true).count();
        let visible = if self.signal.does_archive_show { archived } else { 0 };
        FIRST_CONVERSATION + self.signal.summaries.len() - archived + usize::from(archived > 0) + visible
    }

    fn signal_place(&self, at: usize) -> Option<Place<'_>> {
        if self.signal.is_choosing {
            return self.signal.contacts.as_ref()?.get(at).map(Place::Contact);
        }
        let Some(at) = at.checked_sub(FIRST_CONVERSATION) else {
            return Some(Place::New);
        };
        let unarchived = self.signal.summaries.len() - self.summaries_of(true).count();
        if at < unarchived {
            return self.summaries_of(false).nth(at).map(Place::Conversation);
        }
        match at - unarchived {
            0 => (unarchived < self.signal.summaries.len()).then(|| Place::Archived(self.summaries_of(true).count())),
            offset => {
                let archived = self.summaries_of(true).nth(offset - 1);
                archived.filter(|_| self.signal.does_archive_show).map(Place::Conversation)
            }
        }
    }

    fn deal_signal(&mut self) {
        self.lay_lines(List::Signal, self.signal_count());
    }

    pub(crate) fn signal_item(&self, index: usize) -> Option<Item> {
        let summary = match self.signal_place(index)? {
            Place::New => return Some(Item::lead(NEW_MESSAGE, TO_A_CONTACT).with_icon(NEW_ICON)),
            Place::Archived(count) => {
                let chats = if count == 1 { CHAT } else { CHATS };
                return Some(Item::lead(ARCHIVED, format!("{count} {chats}")).with_icon(ARCHIVED_ICON));
            }
            Place::Contact(contact) => return Some(contact_item(contact)),
            Place::Conversation(summary) => summary,
        };
        let mut word = String::new();
        if summary.unread > 0 {
            let _ = write!(word, "{} {NEW_MARK}", summary.unread);
        } else if summary.is_marked {
            word.push_str(NEW_MARK);
        }
        if let Some(last) = &summary.last {
            if last.is_owner {
                word.push_str(YOU);
                word.push_str(AFTER_NAME);
            }
            word.push_str(&to_entry_text(last));
        }
        let word = wrap_lines(&word, self.line_room(List::Signal), 0).into_iter().next().unwrap_or_default();
        Some(Item::lead(name_of(summary.name.as_deref()), word).with_icon(icon_of(summary.peer)))
    }

    pub(super) fn pick_signal(&mut self) {
        let peer = match self.signal_place(self.signal.grid.selection) {
            Some(Place::New) => return self.choose_contact(),
            Some(Place::Archived(_)) => None,
            Some(Place::Conversation(summary)) => Some(summary.peer),
            Some(Place::Contact(contact)) => Some(contact.peer),
            None => return,
        };
        match peer {
            Some(peer) => {
                self.signal.is_choosing = false;
                self.open_conversation(peer);
                if let Some(pending) = self.signal.pending.take() {
                    (self.signal.carried, self.signal.room.at) = (Some(pending), 0);
                    self.lay_conversation_still();
                }
            }
            None => {
                self.signal.does_archive_show = !self.signal.does_archive_show;
                self.signal.grid.set_items(self.signal_count());
                self.deal_lines(List::Signal, |_| true);
            }
        }
    }

    fn open_conversation(&mut self, peer: Peer) {
        if self.signal.peer != Some(peer) {
            (self.signal.conversation, self.signal.draft, self.signal.voice.phase) = (None, None, Phase::Idle);
            (self.signal.lines, self.signal.height, self.signal.carried) = (Vec::new(), 0, None);
        }
        (self.signal.peer, self.signal.is_awaited) = (Some(peer), true);
        self.screen = Some(Screen::Conversation);
        self.signal.room.at = 0;
        self.lay_conversation();
        (self.signal.room.scroll, self.signal.band) =
            (Edge::still(self.conversation_aim()), Edge::still(self.conversation_band_aim()));
        self.signal.room.plate.make_room(self.ruler.room_size());
        if self.signal.is_typing {
            ready(&mut self.signal.keys, MAX_TEXT_CHARS);
        }
        self.signal.commands.push(Command::Read(peer));
    }

    // Opens a conversation as a hand would, from Signal's list, with a draft under its messages for a click to send; nothing opens while the R1 is not linked.
    pub(super) fn open_draft(&mut self, peer: Peer, draft: String, now_ns: u64) -> Option<Action> {
        let action = self.reach(&|content| content.runs(Use::Open(Destination::Signal)), true, now_ns);
        if self.screen == Some(Screen::Signal) {
            self.depart(FORWARD);
            self.open_conversation(peer);
            (self.signal.draft, self.signal.draft_peer, self.signal.room.at) = (Some(draft), Some(peer), 0);
            self.lay_conversation_still();
        }
        action
    }

    /// Has an item of a roll sent on Signal, where a video larger than Signal's apps send goes nowhere and the roll says so; true if the screen changed.
    #[must_use]
    pub fn send_item(&mut self, sending: Sending, now_ns: u64) -> bool {
        let Some(enclosure) = sending.to_enclosure() else {
            return self.set_note(TOO_LARGE.into(), false, now_ns);
        };
        self.carry_to_signal(enclosure, 0, now_ns);
        true
    }

    // Has a file sent on Signal, a voice note with its seconds: the conversations open as a hand would open them, the bar asking where it goes, and the one chosen, or a contact's, opens with it under the draft for a click; nothing waits while the R1 is not linked.
    pub(super) fn carry_to_signal(&mut self, enclosure: Enclosure, seconds: u64, now_ns: u64) {
        // Opening Signal asks nothing of the device.
        let _ = self.reach(&|content| content.runs(Use::Open(Destination::Signal)), true, now_ns);
        if self.screen == Some(Screen::Signal) {
            self.signal.pending = Some((enclosure, seconds));
        }
    }

    // True where Signal's settings show the photos and voice notes a conversation holds, not text alone.
    fn is_signal_media(&self) -> bool {
        !self.settings.is_text_only
    }

    // A photo that went up is held in its message's own file: its picture follows it there, unchanged, and stays for the file it went up from while the conversation or the draft still shows that one.
    fn follow_moved_photos(&mut self, moved: Vec<(String, String)>) {
        for (from, to) in moved {
            let Some(at) = self.signal.pictures.iter().position(|(file, _)| *file == from) else { continue };
            if self.signal.pictures.iter().any(|(file, _)| *file == to) {
                continue;
            }
            if self.signal_photos().any(|file| file == from) {
                let picture = self.signal.pictures[at].1.clone();
                self.signal.pictures.push((to, picture));
            } else {
                self.signal.pictures[at].0 = to;
            }
        }
    }

    // How tall a photo stands under its message: as its picture once decoded, none where it could not be, which leaves it told by its word, and until then as its told size has it at the room's width, so that its picture moves no row as it comes.
    fn photo_height(&self, file: Option<&str>, size: (usize, usize)) -> Option<i32> {
        let decoded = file.and_then(|file| self.signal.pictures.iter().find(|(held, _)| held == file));
        let height = match decoded {
            Some((_, picture)) => picture.as_ref()?.height(),
            None => standing_height(size, usize::try_from(self.conversation_rest_room().w).ok()?)?,
        };
        i32::try_from(height).ok()
    }

    fn lay_entry(
        &self,
        entry: &Entry,
        speaker: &str,
        (lines, top): (&mut Vec<Line>, &mut i32),
        voice_rows: &mut Vec<(String, i32)>,
    ) {
        let conversation = self.signal.conversation.as_ref();
        let writer = |sender| name_of(conversation.and_then(|view| view.writers.get(&sender)).map(String::as_str));
        let (kind, side_kind) =
            if entry.is_owner { (LineKind::You, LineKind::Note) } else { (LineKind::Robot, LineKind::Aside) };
        if let Some(quote) = &entry.quote {
            let is_owner = conversation.is_some_and(|view| view.owner == quote.author);
            let author = if is_owner { YOU } else { writer(quote.author) };
            let first_line = quote.text.lines().next().unwrap_or_default();
            let quote_line = format!("{REPLYING}{author}{AFTER_NAME}{first_line}");
            let quote_line = cut(&quote_line, cells(self.conversation_rest_room().w, &self.ruler.body));
            self.talk_form().lay(quote_line, (side_kind, speaker), true, lines, top);
        }
        self.talk_form().lay(entry_words(entry), (kind, speaker), entry.deleter.is_some(), lines, top);
        let is_media = self.is_signal_media() && entry.deleter.is_none();
        let mut photos = 0;
        for held in entry.attachments.iter().filter(|held| is_media && held.is_drawn()) {
            let file = held.file.as_deref();
            if let Some(height) = self.photo_height(file, held.size()) {
                let named = (file.unwrap_or_default(), placeholder_word(Placeholder::Photo));
                self.talk_form().push_picture(lines, top, named, height);
                photos += 1;
            }
        }
        let carried = entry.placeholders.iter().filter(|placeholder| {
            let is_shown = **placeholder == Placeholder::Photo && photos > 0;
            photos -= usize::from(is_shown);
            !is_shown
        });
        let voices = entry.attachments.iter().filter(|held| is_media && held.is_voice);
        let voices: Vec<&str> = voices.filter_map(|held| held.file.as_deref()).collect();
        let is_playing = self.signal.playing.as_ref().is_some_and(|(file, _)| voices.contains(&file.as_str()));
        let status = match (entry.is_owner, entry.status) {
            (true, Status::Sending) => Some(SENDING_NOTE),
            (true, Status::Failed) => Some(NOT_SENT_NOTE),
            (true, Status::Delivered) => Some(DELIVERED_NOTE),
            (true, Status::Read) => Some(READ_NOTE),
            _ => None,
        };
        let reactions = to_reactions_note(entry);
        let told: Vec<&str> = (entry.sender.map(writer).into_iter())
            .chain(carried.map(|placeholder| placeholder_word(*placeholder)))
            .chain(is_playing.then_some(PLAYING_NOTE))
            .chain(status)
            .chain(entry.is_robot.then_some(AUTO_NOTE))
            .chain(entry.is_edited.then_some(EDITED_NOTE))
            .chain((entry.deleter == Some(Deleter::Admin)).then_some(BY_ADMIN_NOTE))
            .chain(Some(reactions.as_str()).filter(|reactions| !reactions.is_empty()))
            .collect();
        if !told.is_empty() {
            self.talk_form().push(lines, top, side_kind, Row::plain(told.join(", "), true));
        }
        voice_rows.extend(voices.into_iter().map(|file| (String::from(file), *top)));
    }

    // The draft under the messages: its words, the photo it carries standing whole, or the word of what it carries, a voice note's with its seconds.
    fn lay_draft(&self, speaker: &str, lines: &mut Vec<Line>, top: &mut i32) {
        let signal = &self.signal;
        if signal.draft.is_none() && signal.carried.is_none() {
            return;
        }
        let talk = self.talk_form();
        if let Some(draft) = &signal.draft {
            talk.lay(draft, (LineKind::You, speaker), true, lines, top);
        }
        let mut told = Vec::new();
        if let Some((enclosure, seconds)) = &signal.carried {
            let word = placeholder_word(enclosure.placeholder());
            let height = self.photo_height(Some(&enclosure.file), enclosure.size());
            match height.filter(|_| self.is_signal_media()) {
                Some(height) => talk.push_picture(lines, top, (&enclosure.file, word), height),
                None if enclosure.is_voice => told.push(format!("{word} {seconds} {SECONDS}")),
                None => told.push(String::from(word)),
            }
        }
        told.push(String::from(DRAFT_NOTE));
        talk.push(lines, top, LineKind::Note, Row::plain(told.join(", "), true));
    }

    fn lay_conversation(&mut self) {
        let previous = self.signal.height;
        let conversation = self.signal.conversation.as_ref();
        let name = conversation.map_or(UNNAMED, |view| name_of(view.name.as_deref()));
        let (speaker, title) = (to_speaker(name), name.to_uppercase());
        let (mut lines, mut top) = (core::mem::take(&mut self.signal.lines), 0);
        lines.clear();
        let (mut day, mut voice_rows) = (String::new(), Vec::new());
        for entry in conversation.map_or(&[][..], |view| &view.entries[..]) {
            let seconds = to_local(entry.at / MILLIS_PER_SECOND, self.settings.offset);
            let entry_day = Moment::from_seconds(seconds).to_compact_date();
            if entry_day != day {
                self.talk_form().lay_day(&entry_day, &mut lines, &mut top);
                day = entry_day;
            }
            self.lay_entry(entry, &speaker, (&mut lines, &mut top), &mut voice_rows);
        }
        if conversation.is_some_and(|view| view.is_safety_changed) {
            let talk = self.talk_form();
            talk.push(&mut lines, &mut top, LineKind::Day, Row::gap());
            talk.push(&mut lines, &mut top, LineKind::Day, Row::plain(String::from(SAFETY_CHANGED), false));
        }
        self.lay_draft(&speaker, &mut lines, &mut top);
        if self.signal.voice.is_listening() {
            self.talk_form().push_meter(&mut lines, &mut top);
        }
        // A voice note that is held no more, deleted or gone with its message, plays no more.
        if self.signal.playing.as_ref().is_some_and(|(file, _)| voice_rows.iter().all(|(held, _)| held != file)) {
            self.signal.playing = None;
        }
        (self.signal.lines, self.signal.height, self.signal.speaker, self.signal.title) = (lines, top, speaker, title);
        self.signal.notes = voice_rows;
        self.signal.room.plate.key = None;
        if self.signal.room.at > 0 {
            self.signal.room.at =
                (self.signal.room.at + self.signal.height - previous).clamp(0, self.conversation_max_scroll());
        }
    }

    // What comes whole or takes a place among the messages, a conversation as it opens and a photo decoded, scrolls nothing into view: the scroll moves with its aim at once.
    fn lay_conversation_still(&mut self) {
        let aim = self.conversation_aim();
        self.lay_conversation();
        self.signal.room.scroll.at += self.conversation_aim() - aim;
    }

    pub(in crate::ui) fn conversation_rows(&self) -> (&[Line], &str, &[Photo]) {
        (&self.signal.lines[..], self.signal.speaker.as_str(), &self.signal.pictures[..])
    }

    pub(super) fn press_conversation(&mut self, now_ns: u64) {
        self.hold_start = Some(now_ns);
        if !self.signal.is_typing {
            (self.signal.voice.is_talking, self.signal.draft_peer) = (true, self.signal.peer);
            self.signal.bids.push(Bid::Listen);
            self.signal.hushed = self.signal.playing.take().map(|(file, _)| file);
            if self.signal.hushed.is_some() {
                self.lay_conversation();
            }
        }
    }

    pub(super) fn release_conversation(&mut self, now_ns: u64) {
        let hold_ns = self.hold_start.take().map(|start_ns| now_ns.saturating_sub(start_ns));
        if self.signal.is_typing {
            if hold_ns.is_some() && self.signal.keys.ask_done() {
                self.send_conversation();
            }
            return;
        }
        if !core::mem::take(&mut self.signal.voice.is_talking) {
            return;
        }
        let hushed = self.signal.hushed.take();
        if !hold_ns.is_some_and(|hold_ns| hold_ns < HOLD_TO_TALK_NS) {
            return self.signal.bids.push(Bid::Send);
        }
        self.signal.bids.push(Bid::Drop);
        if self.signal.draft.is_some() || self.signal.carried.is_some() {
            let draft = self.signal.draft.take().unwrap_or_default();
            self.send_message(draft);
        } else if let Some(file) = self.nearest_note() {
            if hushed.as_ref() != Some(&file) {
                self.signal.plays = self.signal.plays.wrapping_add(1);
                self.signal.playing = Some((file, self.signal.plays));
                self.lay_conversation();
            }
        } else {
            self.signal.room.scroll.speed -= SCROLLING.launch_speed(self.ruler.gap as f32);
        }
    }

    // The voice note whose message's foot stands nearest the foot of the room as it shows.
    fn nearest_note(&self) -> Option<String> {
        let (foot, offset) = (self.conversation_visible_room().bottom(), round(self.signal.room.scroll.at));
        let distance = |row: i32| (self.ruler.bar() + row - offset - foot).abs();
        self.signal.notes.iter().min_by_key(|(_, row)| distance(*row)).map(|(file, _)| file.clone())
    }

    pub(super) fn send_conversation(&mut self) {
        if let Some(text) = take_typed(&mut self.signal.keys) {
            self.send_message(text);
        }
    }

    // Sends the words with the file the draft carries.
    fn send_message(&mut self, body: String) {
        let Some(peer) = self.signal.peer else {
            return;
        };
        self.signal.room.at = 0;
        let enclosures = self.signal.carried.take().map(|(enclosure, _)| enclosure).into_iter().collect();
        self.signal.commands.push(Command::Send(Outgoing { peer, body, is_robot: false, enclosures }));
        self.lay_conversation();
    }

    pub(super) fn toggle_conversation_keys(&mut self, now_ns: u64) {
        if core::mem::take(&mut self.signal.voice.is_talking) {
            self.signal.bids.push(Bid::Drop);
            self.hold_start = None;
        }
        self.signal.is_typing = !self.signal.is_typing;
        raise(&mut self.signal.keys, self.signal.is_typing, MAX_TEXT_CHARS);
        self.signal.room.at = self.signal.room.at.min(self.conversation_max_scroll());
        self.hush_robot(self.signal.is_typing, now_ns);
    }

    pub(super) fn switch_auto(&mut self, now_ns: u64) {
        let Some(Peer::Contact(contact)) = self.signal.peer else {
            return;
        };
        let is_on = self.signal.auto != Some(contact);
        self.signal.auto = is_on.then_some(contact);
        self.signal.commands.push(Command::Auto(self.signal.auto));
        self.react(if is_on { Reaction::Cheer } else { Reaction::Nod }, now_ns);
    }

    // A draft goes with the file it carries.
    pub(super) fn drop_draft(&mut self) -> bool {
        if !self.does_conversation_show() || self.signal.draft.is_none() && self.signal.carried.is_none() {
            return false;
        }
        (self.signal.draft, self.signal.carried) = (None, None);
        self.lay_conversation();
        true
    }

    pub(super) fn go_back_to_signal(&mut self) {
        self.leave_screen();
        self.open_signal_list();
    }

    pub(in crate::ui) fn leave_conversation(&mut self) {
        if self.does_conversation_show() {
            self.signal.keys.release();
            self.let_go_conversation();
            self.signal.pictures.clear();
        }
    }

    // A recording that runs is given up, and a voice note that plays stops.
    pub(crate) fn let_go_conversation(&mut self) {
        if core::mem::take(&mut self.signal.voice.is_talking) {
            self.signal.bids.push(Bid::Drop);
        }
        if self.signal.playing.take().is_some() {
            self.lay_conversation();
        }
    }

    /// The voice note to play, by its file and the number of its playing, while its conversation shows on a device awake.
    pub fn signal_playing(&self) -> Option<(&str, u32)> {
        let playing = self.signal.playing.as_ref().filter(|_| self.are_drafts_wanted());
        playing.map(|(file, round)| (file.as_str(), *round))
    }

    // True while a voice note of Signal's plays.
    pub(super) fn is_signal_sounding(&self) -> bool {
        self.signal_playing().is_some()
    }

    // Takes the word that the playing of that number ended, at the voice note's end or as it could not be played; true if the screen changed.
    #[must_use]
    fn set_signal_played(&mut self, round: u32) -> bool {
        if self.signal.playing.as_ref().is_none_or(|(_, playing)| *playing != round) {
            return false;
        }
        self.signal.playing = None;
        self.lay_conversation();
        self.does_conversation_show()
    }

    /// Takes a said message's phase and its draft; true if the screen changed.
    #[must_use]
    pub fn set_signal_draft(&mut self, phase: Phase, draft: Option<String>, now_ns: u64) -> bool {
        let was_listening = self.signal.voice.is_listening();
        let has_new_phase = self.take_phase(VoiceId::Signal, phase, now_ns);
        if self.signal.voice.is_listening() != was_listening {
            self.lay_conversation();
        }
        let has_draft = draft.is_some();
        let is_same_peer = self.signal.draft_peer == self.signal.peer;
        if let Some(draft) = draft.filter(|_| is_same_peer) {
            self.signal.draft = Some(draft);
            self.signal.room.at = 0;
            self.lay_conversation();
            self.react(Reaction::Nod, now_ns);
        }
        (has_new_phase || has_draft) && self.does_conversation_show()
    }

    /// Takes a voice note said for the conversation that shows: it waits under the draft for a click in place of what waited there; one said for another goes nowhere. True if the screen changed.
    #[must_use]
    pub fn set_signal_note(&mut self, note: Option<Recording>, now_ns: u64) -> bool {
        let Some(note) = note.filter(|_| self.signal.draft_peer == self.signal.peer) else {
            return false;
        };
        (self.signal.carried, self.signal.room.at) = (Some((Enclosure::said(note.sound), note.seconds)), 0);
        self.lay_conversation();
        self.react(Reaction::Nod, now_ns);
        self.does_conversation_show()
    }

    // The files of the photos the conversation that shows and its draft carry, as far as the R1 holds them, from the foot up, the order their pictures are decoded in: the draft's, then the newest message's first; none where text alone shows.
    fn signal_photos(&self) -> impl Iterator<Item = &str> {
        let is_media = self.is_signal_media();
        let views = self.signal.conversation.iter().filter(move |_| is_media);
        let entries = views.flat_map(|view| view.entries.iter().rev());
        let held = entries.flat_map(|entry| entry.attachments.iter().rev()).filter(|held| held.is_drawn());
        let is_photo = |enclosure: &Enclosure| enclosure.placeholder() == Placeholder::Photo;
        let carried = self.signal.carried.iter().filter(move |(enclosure, _)| is_media && is_photo(enclosure));
        carried.map(|(enclosure, _)| enclosure.file.as_str()).chain(held.filter_map(|held| held.file.as_deref()))
    }

    /// The photos whose pictures are wanted, with the width they stand at, the room's: those of the conversation that shows on a device awake that were not decoded at that width yet.
    pub fn signal_pictures_wanted(&self) -> Option<(Vec<String>, usize)> {
        let wide = usize::try_from(self.conversation_rest_room().w).ok().filter(|_| self.are_drafts_wanted())?;
        let is_decoded =
            |file: &str| wide == self.signal.pictured && self.signal.pictures.iter().any(|(held, _)| held == file);
        let mut files: Vec<String> = Vec::new();
        for file in self.signal_photos().filter(|file| !is_decoded(file)) {
            if !files.iter().any(|held| held == file) {
                files.push(String::from(file));
            }
        }
        (!files.is_empty()).then_some((files, wide))
    }

    // Takes the pictures the photos' files were decoded to for a width, none of one that could not be, which stays told by its word; those of another width than the room's are let go. True if the screen changed.
    #[must_use]
    fn set_signal_pictures(&mut self, (drawn, wide): (Vec<Photo>, usize)) -> bool {
        if !self.does_conversation_show() || usize::try_from(self.conversation_rest_room().w).ok() != Some(wide) {
            return false;
        }
        if self.signal.pictured != wide {
            (self.signal.pictures, self.signal.pictured) = (Vec::new(), wide);
        }
        self.signal.pictures.extend(drawn);
        self.lay_conversation_still();
        self.does_conversation_show()
    }

    pub(super) fn write_signal_word(&self, text: &mut String) {
        match (self.signal.link_status, self.signal.unread) {
            (LinkStatus::Linked, 0) => text.push_str(ALL_READ),
            (LinkStatus::Linked, unread) => {
                let _ = write!(text, "{unread} {UNREAD}");
            }
            _ => text.push_str(LINK_A_PHONE),
        }
    }

    pub(super) fn signal_waiting(&self) -> (&str, usize) {
        let mut waiting = self.signal.summaries.iter().filter(|summary| summary.is_counted && summary.unread > 0);
        match waiting.next() {
            Some(first) => (name_of(first.name.as_deref()), 1 + waiting.count()),
            None => ("", 0),
        }
    }

    fn link_word(&self) -> &str {
        match &self.signal.link_step {
            LinkStep::Failed(cause) => cause_word(*cause),
            LinkStep::Code(_) if self.signal.code.is_some() => SCAN,
            LinkStep::MakingKeys => LINKING,
            _ => CONNECTING,
        }
    }

    pub(super) fn view_link(&self) -> View<'_> {
        let words = BarWords { status: Some(self.link_word()), ..BarWords::default() };
        let is_code = matches!(self.signal.link_step, LinkStep::Code(_));
        let code = self.signal.code.as_ref().filter(|_| is_code).map(|code| &code.1);
        View::Link { name: SIGNAL, words, code, caption: WHERE }
    }

    // What a list Signal's socket serves tells while the socket does not stand: that it connects, or why it failed.
    pub(super) fn signal_status(&self) -> Option<&'static str> {
        match (self.signal.is_connected, self.signal.failure) {
            (false, Some(cause)) => Some(cause_word(cause)),
            (false, None) => Some(CONNECTING),
            (true, _) => None,
        }
    }

    pub(super) fn view_signal(&self) -> View<'_> {
        let count = self.signal_count();
        let status = self.signal_status();
        let (name, none) = if self.signal.is_choosing {
            (NEW_MESSAGE, self.signal.contacts.as_ref().is_some_and(Vec::is_empty).then_some(NO_CONTACTS))
        } else {
            (SIGNAL, self.signal.summaries.is_empty().then_some(NO_MESSAGES))
        };
        let words = BarWords {
            status,
            subject: none.filter(|_| status.is_none()),
            place: (count > 0).then(|| (self.signal.grid.selection + 1, count)),
            ..BarWords::default()
        };
        let name = if self.signal.pending.is_some() { SEND_TO } else { name };
        View::Lines { list: List::Signal, name, words }
    }

    fn write_conversation_word(&self, now_ns: u64, text: &mut String) {
        let signal = &self.signal;
        let is_sending = signal.conversation.as_ref().is_some_and(|view| {
            view.entries.iter().rev().take_while(|entry| entry.is_owner).any(|entry| entry.status == Status::Sending)
        });
        let is_aloud = self.signal_aloud().is_some();
        let is_auto = signal.auto.is_some() && signal.auto.map(Peer::Contact) == signal.peer;
        match signal.voice.phase {
            Phase::Idle if is_aloud && signal.reading_phase == Phase::Connecting => text.push_str(CONNECTING),
            Phase::Idle if is_aloud && signal.reading_phase == Phase::Speaking => text.push_str(READING),
            Phase::Idle if !signal.is_connected && signal.failure.is_some() => {
                text.push_str(signal.failure.map_or("", cause_word));
            }
            Phase::Idle if is_auto => text.push_str(AUTO),
            Phase::Idle if is_sending => text.push_str(SENDING),
            Phase::Idle if signal.is_typing => text.push_str(TYPE_A_MESSAGE),
            Phase::Idle if signal.draft.is_some() || signal.carried.is_some() => text.push_str(DRAFTED),
            Phase::Idle => text.push_str(HOLD_TO_TALK),
            _ => {
                signal.voice.write_word(now_ns, text);
            }
        }
    }

    pub(crate) fn land_conversation(&mut self) {
        self.signal.keys.land();
        (self.signal.room.scroll, self.signal.band) =
            (Edge::still(self.conversation_aim()), Edge::still(self.conversation_band_aim()));
        self.signal.room.plate.land();
    }

    pub(super) fn view_conversation<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        self.write_conversation_word(now_ns, word);
        let status: &'a str = word;
        let words = BarWords {
            status: Some(status).filter(|status| !status.is_empty()),
            state: Some(if self.signal.is_typing { KEYS } else { VOICE }),
            ..BarWords::default()
        };
        let has_disc = !self.signal.is_typing;
        let keys = Some((KeyboardId::Signal, self.signal.band.at));
        View::Conversation {
            name: &self.signal.title,
            words,
            room: self.signal.room.plate.as_room(),
            keys,
            is_lit: has_disc.then_some(self.signal.voice.is_talking),
        }
    }
}
