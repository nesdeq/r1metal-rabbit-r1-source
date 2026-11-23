use super::bar::BarWords;
use super::{FORWARD, Round, Stopwatch, View, seconds_since};
use crate::base::time::write_duration;
use crate::domain::dialogue::Turn;
use crate::domain::paper::rows::wrap_lines;
use crate::domain::scene::TRANSLATOR;
use crate::domain::translator::state::Report;
use crate::domain::voice::Phase;
use crate::gfx::motion::Edge;
use crate::ui::Ui;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::textroom::{Plate, TextRoom};
use crate::ui::words::{CONNECTING, ENDED, ENDING, LISTENING, cause_word};
use alloc::string::String;
use alloc::vec::Vec;

const SPEAKING: &str = "SPEAKING";
const TRANSLATION_INDENT: usize = 2;

pub(crate) struct Translator {
    round: Round,
    pub(super) session: Phase,
    pub(crate) running: Stopwatch,
    last_seconds: Option<u64>,
    turns: Vec<Turn>,
    turn_lines: Vec<usize>,
    pub(crate) remarks: Vec<(String, bool)>,
    pub(in crate::ui) room: TextRoom<i32>,
}

impl Translator {
    pub(crate) fn new() -> Translator {
        Translator {
            round: Round::default(),
            session: Phase::Idle,
            running: Stopwatch::default(),
            last_seconds: None,
            turns: Vec::new(),
            turn_lines: Vec::new(),
            remarks: Vec::new(),
            room: TextRoom::default(),
        }
    }

    // The rows the microphone's level takes under the remarks: one while a session stands, whether it listens or speaks, so that the remarks keep their place between the two.
    pub(in crate::ui) fn meter_rows(&self) -> usize {
        usize::from(self.session.is_session_up())
    }
}

impl Ui {
    /// The number of the session the translator's screen holds.
    pub fn session_request(&self) -> Option<u32> {
        self.translator.round.current().filter(|_| self.is_awake() && self.screen == Some(Screen::Talk))
    }

    pub(crate) fn is_billed(&self) -> bool {
        self.session_request().is_some()
    }

    pub(crate) const fn end_billed(&mut self) {
        self.translator.round.is_on = false;
    }

    fn begin_talk(&mut self) {
        let translator = &mut self.translator;
        translator.round.begin();
        (translator.session, translator.running.start_ns, translator.last_seconds) = (Phase::Connecting, None, None);
        translator.turns.clear();
        translator.turn_lines.clear();
        translator.remarks.clear();
        (translator.room.at, translator.room.scroll, translator.room.plate.key) = (0, Edge::still(0.0), None);
    }

    pub(super) fn open_talk(&mut self) {
        self.depart(FORWARD);
        self.screen = Some(Screen::Talk);
        self.begin_talk();
        self.translator.room.plate.make_room(self.ruler.room_size());
    }

    pub(super) fn click_talk(&mut self) {
        if self.translator.round.is_on {
            self.translator.round.is_on = false;
        } else {
            self.begin_talk();
            self.translator.room.plate.renew(self.ruler.room_size(), true);
        }
    }

    /// Takes the phase of the session of that number; true if the screen changed.
    #[must_use]
    pub fn set_session(&mut self, round: u32, session: Phase, now_ns: u64) -> bool {
        let translator = &mut self.translator;
        if !translator.round.note_phase(round, translator.session, session) {
            return false;
        }
        if session.is_session_up() && !translator.session.is_session_up() {
            (translator.running.start_ns, translator.last_seconds) = (Some(now_ns), None);
        }
        if !session.is_session_up() && session != Phase::Connecting {
            let seconds = translator.running.start_ns.take().map(|start_ns| seconds_since(start_ns, now_ns));
            translator.last_seconds = seconds.or(translator.last_seconds);
        }
        translator.session = session;
        if matches!(session, Phase::Failed(_)) {
            self.react(Reaction::Slump, now_ns);
        }
        self.screen == Some(Screen::Talk)
    }

    /// Takes the turns of the session of that number; true if the screen changed.
    #[must_use]
    pub fn set_remarks(&mut self, round: u32, turns: &[Turn], now_ns: u64) -> bool {
        let cells = self.ruler.text_cells().0.0;
        let translator = &mut self.translator;
        let same_count = translator.turns.iter().zip(turns).take_while(|entry| entry.0 == entry.1).count();
        if round != translator.round.number || (same_count == turns.len() && same_count == translator.turns.len()) {
            return false;
        }
        let remarks = &mut translator.remarks;
        remarks.truncate(translator.turn_lines.get(same_count).copied().unwrap_or(remarks.len()));
        translator.turns.truncate(same_count);
        translator.turn_lines.truncate(same_count);
        for turn in &turns[same_count..] {
            translator.turn_lines.push(remarks.len());
            remarks.extend(wrap_lines(&turn.source, cells, 0).into_iter().map(|line| (line, false)));
            remarks
                .extend(wrap_lines(&turn.translation, cells, TRANSLATION_INDENT).into_iter().map(|line| (line, true)));
            translator.turns.push(turn.clone());
        }
        (translator.room.at, translator.room.plate.key, self.used_at) = (0, None, Some(now_ns));
        self.screen == Some(Screen::Talk)
    }

    /// Takes the translator's report; true if the screen changed.
    #[must_use]
    pub fn take_talk_report(&mut self, report: &Report, now_ns: u64) -> bool {
        let has_remarks = report.remarks.as_ref().is_some_and(|(round, turns)| self.set_remarks(*round, turns, now_ns));
        let has_session = report.session.is_some_and(|(round, session)| self.set_session(round, session, now_ns));
        let is_kept = report.kept_count > 0;
        self.memos.file_changes = self.memos.file_changes.wrapping_add(report.kept_count);
        if is_kept {
            self.react(Reaction::Nod, now_ns);
        }
        has_remarks || has_session || is_kept
    }

    pub(super) fn is_talking(&self) -> bool {
        self.session_request().is_some()
            || (self.screen == Some(Screen::Talk) && self.translator.session.is_session_up())
    }

    pub(crate) fn land_talk(&mut self) {
        self.translator.room.scroll = Edge::still(self.push_aim());
        self.translator.room.plate.land();
    }

    pub(super) fn view_talk<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        let translator = &self.translator;
        let seconds = translator.running.start_ns.map(|start_ns| seconds_since(start_ns, now_ns));
        match (translator.session, seconds, translator.last_seconds) {
            (Phase::Failed(cause), ..) => word.push_str(cause_word(cause)),
            (Phase::Connecting, ..) if translator.round.is_on => word.push_str(CONNECTING),
            (session, Some(seconds), _) if session.is_session_up() && translator.round.is_on => {
                word.push_str(if session == Phase::Speaking { SPEAKING } else { LISTENING });
                word.push(' ');
                write_duration(word, seconds);
            }
            (Phase::Idle, _, Some(seconds)) => {
                word.push_str(ENDED);
                word.push(' ');
                write_duration(word, seconds);
            }
            (Phase::Idle, ..) => word.push_str(ENDED),
            _ => word.push_str(ENDING),
        }
        let status: &'a str = word;
        View::Text {
            name: TRANSLATOR,
            words: BarWords { status: Some(status), ..BarWords::default() },
            room: translator.room.plate.as_room(),
            keys: None,
        }
    }
}
