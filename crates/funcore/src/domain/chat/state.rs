//! The state the conversation's threads and the main loop share.

use super::instructions;
use super::instructions::Instructions;
use super::memory::Memory;
use super::message::to_heard_part;
use super::thread::{self, RobotState, Thread};
use super::tools::{Act, Asked};
use super::transcript::Entry;
use super::{InputKind, MAX_TEXT_CHARS};
use crate::base::handoff::Loan;
use crate::domain::aloud::Script;
use crate::domain::camera::Shot;
use crate::domain::memos::Memo;
use crate::domain::settings::{Settings, Tool};
use crate::domain::shelf::Shelf;
use crate::domain::signal::robot::Reply;
use crate::domain::voice::{Job, Keep, Phase, Query, Voice};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// An input of a turn: the robot's first wake, or a message said or typed, with the number of the reply the user cut short for it and the photo that goes with it.
pub enum Input {
    Wake,
    Message { query: Query, cut_reply: Option<u32>, photo: Option<Shot> },
}

/// The input a message came by.
pub const fn to_kind(query: &Query) -> InputKind {
    match query {
        Query::Voice { .. } => InputKind::Voice,
        Query::Text(_) => InputKind::Text,
    }
}

/// What the user has the conversation forget.
#[derive(Clone, PartialEq, Eq)]
pub enum Lapse {
    // A fact, by the line its file keeps it as.
    Fact(String),
    // A day's words and recollection, by its `yyyymmdd`.
    Day(String),
    Thread,
}

// A fact's line is the owner's, which no log tells.
impl core::fmt::Debug for Lapse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Lapse::Fact(_) => f.write_str("Fact"),
            Lapse::Day(day) => f.debug_tuple("Day").field(day).finish(),
            Lapse::Thread => f.write_str("Thread"),
        }
    }
}

/// A request of the conversation's screens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Chore {
    // The days are wanted, and a conversation never held begins.
    Open,
    // The user left the screen: the voice stops and the photo that waits is let go of.
    Hush,
    Mute(bool),
    // The day before this one, as the wheel reaches the oldest shown.
    PreviousDay(String),
    // The facts or the days, for their list.
    List(Shelf),
    Forget(Lapse),
    // A photo taken for the robot, which goes with the next message.
    Show(Shot),
}

/// An event the robot reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Milestone {
    Wake,
    Name,
    Enough,
}

/// The conversation's report to the main loop.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub phase: Option<Phase>,
    /// Whether the robot is muted and its name, where changed.
    pub identity: Option<(bool, Option<String>)>,
    pub(crate) days: Vec<(String, Vec<Entry>)>,
    /// True where no older day has entries.
    pub has_reached_oldest: bool,
    /// The growing entry, where changed.
    pub current: Option<Option<Entry>>,
    pub milestones: Vec<Milestone>,
    pub(crate) acts: Vec<Act>,
    pub(crate) readout: Option<Asked>,
    /// The reply to read by its number, as far as written, where changed.
    pub reply: Option<(u32, Script)>,
    /// A new listing of the facts or the days, newest first.
    pub listing: Option<(Shelf, Vec<Memo>)>,
    /// The days forgotten since the last report.
    pub forgotten: Vec<String>,
    // Whether a photo waits for the next message, where changed.
    pub(crate) has_photo: Option<bool>,
}

/// The conversation's stored values, as its threads hold them.
#[derive(Default)]
pub struct Store {
    pub memory: Memory,
    pub robot: Option<RobotState>,
    pub thread: Thread,
}

impl Store {
    /// True where the robot is muted.
    pub fn is_muted(&self) -> bool {
        self.robot.as_ref().is_some_and(|state| state.is_muted)
    }

    /// Whether the robot is muted, and its name.
    pub fn identity(&self) -> (bool, Option<String>) {
        (self.is_muted(), self.memory.robot_name().map(String::from))
    }

    /// True during the first conversation.
    pub fn is_meeting(&self) -> bool {
        self.robot.as_ref().is_some_and(|state| !state.is_onboarded)
    }

    /// True before the robot's state exists.
    pub const fn lacks_robot(&self) -> bool {
        self.robot.is_none()
    }

    /// The log line of the files read.
    pub fn to_read_line(&self) -> String {
        format!("chat: {}", thread::to_log_line(&self.thread, self.memory.knowledge.len(), self.robot.as_ref()))
    }
}

#[derive(Default)]
enum Loading {
    #[default]
    Unread,
    Reading,
    Read(bool),
}

/// The next step of a thread that needs the conversation's files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Load {
    Read,
    // Whether the files could be read.
    Done(bool),
}

struct Speech {
    round: u32,
    script: Script,
    heard_count: usize,
}

/// The state of the conversation beside its voice job.
#[derive(Default)]
pub struct Turns {
    is_recalling: bool,
    has_look_back_due: bool,
    speeches: Vec<Speech>,
    voice: Option<u32>,
    voice_phase: Option<(u32, Phase)>,
    cut_reply: Option<u32>,
    round: u32,
    /// The answer to the readout asked, from the main loop.
    pub readout_answer: Option<Reply>,
    /// What the owner has set, from the main loop.
    pub settings: Settings,
    photo: Option<Shot>,
    pub report: Report,
    /// The stored values; out while a thread writes them.
    pub store: Loan<Store>,
    /// The model's last instructions; out for a whole turn.
    pub instructions: Loan<Option<Instructions>>,
    // A rebuild asked for while a turn held the instructions.
    is_stale_due: bool,
    loading: Loading,
}

impl Job for Turns {
    type Input = Input;
    type Chore = Chore;
    type Report = Report;
    const KEEP: Keep = Keep::All;
    const IS_KEYED: bool = true;

    fn take_query(&mut self, query: Query) -> Option<Input> {
        let query = match query {
            Query::Text(text) => Query::Text(text.chars().take(MAX_TEXT_CHARS).collect()),
            said @ Query::Voice { .. } => said,
        };
        Some(Input::Message { query, cut_reply: self.cut_reply.take(), photo: self.take_photo() })
    }

    fn hush(&mut self) -> bool {
        let Some(round) = self.voice.take() else {
            return false;
        };
        self.cut_short(round);
        self.voice_phase = None;
        true
    }

    fn is_idle(&self) -> bool {
        !self.is_recalling
    }

    fn report(&mut self, phase: Phase) -> Report {
        let voice_phase = self.voice_phase.filter(|voice| voice.0 == self.round).map(|voice| voice.1);
        let phase = match (phase, voice_phase) {
            (Phase::Idle | Phase::Thinking | Phase::Searching, Some(Phase::Speaking)) => Phase::Speaking,
            (Phase::Idle, Some(Phase::Failed(cause))) => Phase::Failed(cause),
            _ => phase,
        };
        Report { phase: Some(phase), ..core::mem::take(&mut self.report) }
    }
}

impl Turns {
    /// The next step of a thread that needs the files; none while another reads them.
    pub fn next_load(&mut self) -> Option<Load> {
        match self.loading {
            Loading::Unread => {
                self.loading = Loading::Reading;
                Some(Load::Read)
            }
            Loading::Reading => None,
            Loading::Read(is_ready) => Some(Load::Done(is_ready)),
        }
    }

    /// Takes what the files held; none where they could not be read.
    pub fn end_load(&mut self, store: Option<Store>) {
        self.loading = Loading::Read(store.is_some());
        if let Some(store) = store {
            self.store.give(store);
        }
    }

    /// Marks the instructions for a rebuild, now or as the turn that holds them gives them back.
    pub fn mark_stale(&mut self) {
        match self.instructions.get_mut() {
            Some(instructions) => instructions::mark_stale(instructions),
            None => self.is_stale_due = true,
        }
    }

    /// Takes back the instructions a turn held.
    pub fn give_instructions(&mut self, mut instructions: Option<Instructions>) {
        if core::mem::take(&mut self.is_stale_due) {
            instructions::mark_stale(&mut instructions);
        }
        self.instructions.give(instructions);
    }

    pub fn begin_answer(&mut self) {
        self.voice_phase = None;
    }

    /// Takes the mute as the owner set it: a muted robot's voice stops; true where it did.
    pub fn mute(&mut self, is_muted: bool) -> bool {
        is_muted && self.hush()
    }

    fn forget_speech(&mut self, round: u32) {
        self.speeches.retain(|speech| speech.round != round);
    }

    fn cut_short(&mut self, round: u32) {
        if let Some(last) = self.cut_reply.replace(round) {
            self.forget_speech(last);
        }
    }

    fn speech_mut(&mut self, round: u32) -> Option<&mut Speech> {
        self.speeches.iter_mut().find(|speech| speech.round == round)
    }

    fn report_reply(&mut self) {
        let speech = self.speeches.iter().find(|speech| Some(speech.round) == self.voice);
        if let Some(speech) = speech {
            self.report.reply = Some((speech.round, speech.script.clone()));
        }
    }

    /// Starts the voice of the next reply, read in the way `task` says.
    pub fn begin_voice(&mut self, task: String) {
        self.round = self.round.wrapping_add(1);
        if let Some(last) = self.voice.replace(self.round) {
            self.forget_speech(last);
        }
        self.speeches.push(Speech { round: self.round, script: Script::for_reply(task), heard_count: 0 });
        self.voice_phase = None;
        self.report_reply();
    }

    /// Adds a passage to the current reply.
    pub fn add_passage(&mut self, passage: String) {
        if let Some(speech) = self.speech_mut(self.round) {
            speech.script.passages.push(passage);
        }
        self.report_reply();
    }

    /// Ends the current reply's passages.
    pub fn close_voice(&mut self) {
        if let Some(speech) = self.speech_mut(self.round) {
            speech.script.is_growing = false;
        }
        self.report_reply();
    }

    /// Takes the count of a reply's passages heard to their end.
    pub fn set_heard(&mut self, round: u32, count: usize) {
        if let Some(speech) = self.speech_mut(round) {
            speech.heard_count = count;
        }
    }

    /// The heard part of a reply the user cut short, which is dropped; none where they heard all of it.
    pub fn take_heard_part(&mut self, round: u32) -> Option<String> {
        let at = self.speeches.iter().position(|speech| speech.round == round)?;
        let speech = self.speeches.swap_remove(at);
        to_heard_part(&speech.script.passages, speech.heard_count)
    }

    /// Takes the phase of a reply's reading; true where that is news of the last reply.
    pub fn take_voice_phase(&mut self, round: u32, phase: Phase) -> bool {
        if round != self.round || self.voice_phase == Some((round, phase)) {
            return false;
        }
        self.voice_phase = Some((round, phase));
        if matches!(phase, Phase::Idle | Phase::Failed(_)) && self.voice.take_if(|voice| *voice == round).is_some() {
            if matches!(phase, Phase::Failed(_)) { self.cut_short(round) } else { self.forget_speech(round) }
        }
        true
    }

    /// Puts a day into the report, returning the one it replaces to drop outside the lock.
    pub fn show_day(&mut self, day: String, entries: Vec<Entry>) -> Option<(String, Vec<Entry>)> {
        let days = &mut self.report.days;
        let stale = days.iter().position(|known| known.0 == day).map(|index| days.remove(index));
        days.push((day, entries));
        stale
    }

    /// Adds acts for the device; true where any came.
    pub fn add_acts(&mut self, acts: Vec<Act>) -> bool {
        let has_any = !acts.is_empty();
        self.report.acts.extend(acts);
        has_any
    }

    /// Asks the main loop for a readout of the interface's, or for Signal's answer to a question.
    pub fn ask_readout(&mut self, asked: Asked) {
        self.readout_answer = None;
        self.report.readout = Some(asked);
    }

    /// Keeps a photo taken for the robot until a message goes with it; returns the one it replaces, to drop outside the lock.
    pub fn show_photo(&mut self, shot: Shot) -> Option<Shot> {
        self.report.has_photo = Some(true);
        self.photo.replace(shot)
    }

    /// Takes back what a message that came to no words was handed, for the next: the reply cut short where none was cut since, which lets go of its speech, and the photo where none newer waits, which is returned to drop outside the lock.
    pub fn give_back(&mut self, cut_reply: Option<u32>, photo: Option<Shot>) -> Option<Shot> {
        if let Some(round) = cut_reply {
            match self.cut_reply {
                Some(_) => self.forget_speech(round),
                None => self.cut_reply = Some(round),
            }
        }
        if self.photo.is_some() {
            return photo;
        }
        self.report.has_photo = photo.as_ref().map(|_| true).or(self.report.has_photo);
        self.photo = photo;
        None
    }

    /// Lets go of the photo that waits, returned to drop outside the lock.
    pub fn drop_photo(&mut self) -> Option<Shot> {
        let photo = self.photo.take();
        if photo.is_some() {
            self.report.has_photo = Some(false);
        }
        photo
    }

    // The photo that goes with a message: none while the camera tool is off, which lets go of it.
    fn take_photo(&mut self) -> Option<Shot> {
        let photo = self.drop_photo();
        photo.filter(|_| self.settings.tools.is_on(Tool::Camera))
    }
}

type State = Voice<Turns>;

impl State {
    /// Opens the screen: a conversation `is_never_held` begins with the robot's wake; true where it does.
    pub fn open(&mut self, is_never_held: bool) -> bool {
        self.job.has_look_back_due = true;
        let is_wake = is_never_held && !self.has_inputs() && !self.is_working;
        if is_wake {
            self.put(Input::Wake);
            self.phase = Phase::Waking;
            self.job.report.milestones.push(Milestone::Wake);
        }
        is_wake
    }

    /// True once when the past days are due a look while the device is awake.
    pub fn take_look_back(&mut self) -> bool {
        let is_due = self.job.has_look_back_due && self.is_wanted;
        self.job.has_look_back_due &= !is_due;
        is_due
    }

    /// Sets whether a day is being settled; a start needs the device awake.
    pub fn set_recalling(&mut self, is_recalling: bool) -> bool {
        let is_allowed = !is_recalling || self.is_wanted;
        if is_allowed {
            self.job.is_recalling = is_recalling;
        }
        is_allowed
    }

    /// Sets the idle phase, unless the screen shows a failure; true where the phase changed.
    pub fn set_idle(&mut self) -> bool {
        !matches!(self.phase, Phase::Failed(_)) && self.set_phase(Phase::Idle)
    }

    /// Sets the thinking phase, unless the robot wakes; true where the phase changed.
    pub fn set_thinking(&mut self) -> bool {
        self.phase != Phase::Waking && self.set_phase(Phase::Thinking)
    }

    /// The number of the reply to read; none while the device sleeps.
    pub fn voice(&self) -> Option<u32> {
        self.job.voice.filter(|_| self.is_wanted)
    }

    /// True during a turn or a settling: the network stays, asleep too.
    pub fn is_owing(&self) -> bool {
        self.is_working || self.job.is_recalling
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cause::Cause;
    use crate::domain::voice::Pickup;

    const TASK: &str = "Say it.";

    fn report(state: &mut State) -> Report {
        state.take_report()
    }

    #[test]
    fn speaking_and_failed_phases() {
        let mut state = State::new(true);
        state.begin_recording(true, 0);
        assert_eq!(report(&mut state).phase, Some(Phase::Listening));
        state.end_recording();
        state.job.begin_voice(TASK.into());
        assert!(state.job.take_voice_phase(1, Phase::Speaking) && !state.job.take_voice_phase(1, Phase::Speaking));
        assert_eq!(report(&mut state).phase, Some(Phase::Speaking));
        state.set_phase(Phase::Thinking);
        assert_eq!(report(&mut state).phase, Some(Phase::Speaking));
        state.set_phase(Phase::Idle);
        assert!(state.job.take_voice_phase(1, Phase::Failed(Cause::NoAnswer)));
        assert_eq!(report(&mut state).phase, Some(Phase::Failed(Cause::NoAnswer)));
        assert!(!state.job.take_voice_phase(0, Phase::Speaking));
    }

    #[test]
    fn cut_reply_goes_with_next_message() {
        let mut state = State::new(true);
        state.job.begin_voice(TASK.into());
        assert_eq!(state.voice(), Some(1));
        assert!(state.job.hush() && !state.job.hush());
        state.take_text("Stopp".into());
        let Pickup::Work(inputs, true) = state.pick_up(0) else { panic!("what was typed goes") };
        assert!(matches!(inputs[..], [Input::Message { cut_reply: Some(1), .. }]));
        state.job.add_passage("Ja.".into());
        state.job.add_passage("Regen.".into());
        state.job.set_heard(1, 1);
        assert_eq!(state.job.take_heard_part(1).as_deref(), Some("Ja."), "what came after the hush counts too");
        assert_eq!(state.job.take_heard_part(1), None, "once");
        state.is_working = false;
        state.job.begin_voice(TASK.into());
        state.job.take_voice_phase(2, Phase::Idle);
        assert_eq!(state.voice(), None);
        state.take_text("x".into());
        let Pickup::Work(inputs, _) = state.pick_up(0) else { panic!("what was typed goes") };
        assert!(matches!(inputs[..], [Input::Message { cut_reply: None, .. }]));
        assert_eq!(state.job.speeches.len(), 0, "a reply heard to its end is let go of");
    }

    #[test]
    fn reply_report_grows() {
        let mut state = State::new(true);
        state.job.begin_voice(TASK.into());
        let view =
            |state: &mut State| report(state).reply.map(|reply| (reply.0, reply.1.passages.len(), reply.1.is_growing));
        assert_eq!(view(&mut state), Some((1, 0, true)));
        state.job.add_passage("Ja.".into());
        state.job.close_voice();
        assert_eq!(view(&mut state), Some((1, 1, false)));
        assert_eq!(view(&mut state), None, "once");
        state.job.hush();
        state.job.add_passage("Regen.".into());
        assert_eq!(view(&mut state), None, "a reply cut short is no longer to be said");
        state.job.begin_voice(TASK.into());
        assert!(!state.job.mute(false) && state.voice().is_some(), "unmuted, the voice goes on");
        assert!(state.job.mute(true) && state.voice().is_none(), "muted, it stops");
    }

    #[test]
    fn text_limit() {
        let mut state = State::new(true);
        state.take_text("x".repeat(MAX_TEXT_CHARS + 5));
        let Pickup::Work(inputs, _) = state.pick_up(0) else { panic!() };
        assert!(
            matches!(&inputs[..], [Input::Message { query: Query::Text(text), .. }] if text.len() == MAX_TEXT_CHARS)
        );
        assert_eq!(
            [to_kind(&Query::Text(String::new())), to_kind(&Query::Voice { sound: Vec::new(), seconds: 0 })],
            [InputKind::Text, InputKind::Voice]
        );
    }

    #[test]
    fn first_open_wakes() {
        let mut state = State::new(true);
        assert!(state.open(true));
        assert_eq!(state.phase, Phase::Waking);
        assert!(!state.open(true), "what waits is the waking");
        assert!(state.take_look_back() && !state.take_look_back());
        assert_eq!(report(&mut state).milestones, [Milestone::Wake]);
        assert!(!state.set_thinking());
        state.phase = Phase::Failed(Cause::NoKey);
        assert!(!state.set_idle());
    }

    #[test]
    fn settling_needs_awake_device() {
        let mut state = State::new(false);
        assert!(!state.set_recalling(true));
        state.set_wanted(true);
        assert!(state.set_recalling(true) && state.is_owing() && !state.is_off());
        assert!(state.set_recalling(false) && !state.is_owing());
    }

    #[test]
    fn photo_goes_with_next_message() {
        let mut state = State::new(true);
        let shot = |byte| Shot { jpeg: alloc::vec![byte] };
        assert_eq!(state.job.show_photo(shot(1)), None);
        assert_eq!(state.job.show_photo(shot(2)), Some(shot(1)), "the newest photo waits");
        assert_eq!(report(&mut state).has_photo, Some(true));
        state.take_text("Was ist das?".into());
        let Pickup::Work(inputs, _) = state.pick_up(0) else { panic!("what was typed goes") };
        assert!(matches!(&inputs[..], [Input::Message { photo: Some(photo), .. }] if *photo == shot(2)));
        assert_eq!(report(&mut state).has_photo, Some(false), "it no longer waits");
        state.is_working = false;
        state.take_text("Und das?".into());
        let Pickup::Work(inputs, _) = state.pick_up(0) else { panic!("what was typed goes") };
        assert!(matches!(&inputs[..], [Input::Message { photo: None, .. }]), "once");
        state.is_working = false;
        state.job.show_photo(shot(3));
        assert_eq!(state.job.drop_photo(), Some(shot(3)), "leaving lets go of it");
        assert_eq!((state.job.drop_photo(), report(&mut state).has_photo), (None, Some(false)));
        state.job.show_photo(shot(4));
        state.job.settings.tools = state.job.settings.tools.with(Tool::Camera, false);
        state.take_recording(Some((alloc::vec![0], 1)));
        let Pickup::Work(inputs, _) = state.pick_up(0) else { panic!("what was said goes") };
        assert!(
            matches!(&inputs[..], [Input::Message { query: Query::Voice { .. }, photo: None, .. }]),
            "with the camera tool off the photo goes with nothing"
        );
        assert_eq!(state.job.drop_photo(), None, "and is let go of");
    }

    #[test]
    fn wordless_message_gives_back() {
        let mut state = State::new(true);
        let shot = |byte| Shot { jpeg: alloc::vec![byte] };
        let handed = |state: &mut State| {
            state.take_recording(Some((alloc::vec![0], 1)));
            let Pickup::Work(mut inputs, _) = state.pick_up(0) else { panic!("what was said goes") };
            state.is_working = false;
            match inputs.pop() {
                Some(Input::Message { cut_reply, photo, .. }) => (cut_reply, photo),
                _ => panic!("a message"),
            }
        };
        state.job.begin_voice(TASK.into());
        state.job.hush();
        state.job.show_photo(shot(1));
        let (cut_reply, photo) = handed(&mut state);
        assert_eq!((cut_reply, photo.clone()), (Some(1), Some(shot(1))), "handed over with the message");
        report(&mut state);
        assert_eq!(state.job.give_back(cut_reply, photo), None, "a message that came to no words gives them back");
        assert_eq!(report(&mut state).has_photo, Some(true), "the photo waits again");
        assert_eq!(handed(&mut state), (Some(1), Some(shot(1))), "and the next message carries both");
        state.job.begin_voice(TASK.into());
        state.job.hush();
        state.job.show_photo(shot(2));
        assert_eq!(
            state.job.give_back(Some(1), Some(shot(1))),
            Some(shot(1)),
            "a newer photo waits: the one given back is let go of"
        );
        assert_eq!(
            state.job.speeches.iter().map(|speech| speech.round).collect::<Vec<_>>(),
            [2],
            "and a newer reply was cut: the speech of the one given back goes"
        );
        assert_eq!(handed(&mut state), (Some(2), Some(shot(2))));
    }

    #[test]
    fn stale_while_lent() {
        let mut turns = Turns::default();
        let tools = crate::domain::settings::Tools::default();
        turns.instructions.give(Some(Instructions::new("x".into(), tools, core::time::Duration::ZERO)));
        let held = turns.instructions.take().expect("not lent yet");
        turns.mark_stale();
        turns.give_instructions(held);
        let lent = turns.instructions.get_mut().expect("given back");
        assert_eq!(instructions::warm_text(lent, core::time::Duration::ZERO, tools), None, "rebuilt all the same");
        turns.give_instructions(None);
    }

    #[test]
    fn readouts_asked_and_answered() {
        use crate::domain::chat::tools::Readout;
        use crate::domain::signal::robot::Question;
        let mut turns = Turns::default();
        turns.ask_readout(Asked::Signal(Question::Read(None)));
        assert_eq!(turns.report.readout, Some(Asked::Signal(Question::Read(None))));
        turns.readout_answer = Some(Reply { told: "Nothing yet.".into(), draft: None });
        turns.ask_readout(Asked::Device(Readout::Bell));
        assert_eq!(
            (turns.report.readout, turns.readout_answer),
            (Some(Asked::Device(Readout::Bell)), None),
            "an answer to an earlier question is none to the next"
        );
    }

    #[test]
    fn files_read_once() {
        let mut turns = Turns::default();
        assert_eq!(
            (turns.next_load(), turns.next_load()),
            (Some(Load::Read), None),
            "one thread reads the files, the others wait"
        );
        let mut memory = Memory::default();
        memory.save("Likes tea.", true, None, "2026-10-16");
        turns.end_load(Some(Store { memory, ..Store::default() }));
        assert_eq!(turns.next_load(), Some(Load::Done(true)));
        assert_eq!(
            turns.store.take().map(|store| store.memory.core),
            Some(alloc::vec![String::from("Likes tea.")]),
            "what the files held is the store from then on"
        );
        let mut unread = Turns::default();
        unread.next_load();
        unread.end_load(None);
        assert_eq!(
            (unread.next_load(), unread.next_load()),
            (Some(Load::Done(false)), Some(Load::Done(false))),
            "files that could not be read are not read again, and every later thread is told so"
        );
    }

    #[test]
    fn show_day_returns_replaced() {
        let mut turns = Turns::default();
        assert_eq!(turns.show_day("a".into(), Vec::new()), None);
        assert_eq!(turns.show_day("a".into(), Vec::new()), Some(("a".into(), Vec::new())));
        assert_eq!(turns.report.days.len(), 1);
    }
}
