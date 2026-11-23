use super::Round;
use super::memos::Text;
use crate::domain::aloud::state::ReadingRequest;
use crate::domain::aloud::{ReadingSource, Words};
use crate::domain::shelf::Shelf;
use crate::domain::voice::Phase;
use crate::ui::Ui;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::words::{CONNECTING, READING, cause_word};

const PAUSED: &str = "PAUSED";

pub(crate) struct Aloud {
    round: Round,
    is_held: bool,
    start_block: usize,
    pub(super) phase: Phase,
}

impl Aloud {
    pub(crate) fn new() -> Aloud {
        Aloud { round: Round::default(), is_held: false, start_block: 0, phase: Phase::Idle }
    }
}

impl Ui {
    // What a touch on the robot reads aloud on the screen that shows, with the block at the top of its room: a rundown or a summary the reader shows, or the browser's page.
    fn aloud_script(&self) -> Option<(ReadingSource, &str, usize)> {
        if let Some((text, block)) = self.web_script() {
            return Some((ReadingSource::Web, text, block));
        }
        let (shelf, is_summary) = (self.memos.shelf, self.reader.text == Some(Text::Summary));
        let is_read = self.screen == Some(Screen::Reader) && !shelf.is_robots() && (shelf == Shelf::News || is_summary);
        let block = self.reader.rows.top_block(self.reader.room.at);
        is_read.then(|| (ReadingSource::Text(shelf), self.script_text().unwrap_or_default(), block))
    }

    pub(super) fn does_read_aloud(&self) -> bool {
        self.aloud_script().is_some()
    }

    fn aloud_request(&self) -> Option<(u32, bool)> {
        let round = self.aloud.round.current().filter(|_| self.is_awake() && self.does_read_aloud());
        round.map(|round| (round, self.aloud.is_held))
    }

    pub(in crate::ui) fn is_voiced(&self) -> bool {
        self.aloud_request().is_some_and(|request| !request.1)
    }

    /// The reader's request, `reply` the number of the conversation's reply to be said.
    pub fn reading_request(&self, reply: Option<u32>) -> Option<ReadingRequest<'_>> {
        let text = self.aloud_request().zip(self.aloud_script()).map(|((round, is_held), (source, text, _))| {
            ReadingRequest { round, source, is_held, words: Some(Words::Text(text, self.aloud.start_block)) }
        });
        let reply =
            reply.map(|round| ReadingRequest { round, source: ReadingSource::Reply, is_held: false, words: None });
        let signal = self.signal_reading().map(|(round, passages)| ReadingRequest {
            round,
            source: ReadingSource::Signal,
            is_held: false,
            words: Some(Words::Arrived(passages)),
        });
        text.or(reply).or(signal)
    }

    pub(super) fn touch_aloud(&mut self) {
        if self.aloud.round.is_on {
            self.aloud.is_held = !self.aloud.is_held;
        } else if let Some(start_block) = self.aloud_script().map(|script| script.2) {
            self.aloud.round.begin();
            (self.aloud.is_held, self.aloud.start_block, self.aloud.phase) = (false, start_block, Phase::Connecting);
        }
    }

    pub(crate) fn hush(&mut self) {
        (self.aloud.round.is_on, self.aloud.is_held, self.aloud.phase) = (false, false, Phase::Idle);
    }

    /// Takes the phase of a reading; true if the screen changed.
    #[must_use]
    pub fn set_reading_phase(&mut self, round: u32, phase: Phase, now_ns: u64) -> bool {
        if !self.aloud.round.note_phase(round, self.aloud.phase, phase) {
            return false;
        }
        self.aloud.is_held &= self.aloud.round.is_on;
        if matches!(phase, Phase::Failed(_)) {
            self.react(Reaction::Slump, now_ns);
        }
        self.aloud.phase = phase;
        matches!(self.screen, Some(Screen::Reader | Screen::Web))
    }

    pub(super) fn aloud_word(&self) -> Option<&'static str> {
        match (self.aloud_request(), self.aloud.phase) {
            (_, Phase::Failed(cause)) if self.does_read_aloud() => Some(cause_word(cause)),
            (Some((_, true)), _) => Some(PAUSED),
            (Some((_, false)), Phase::Speaking) => Some(READING),
            (Some((_, false)), _) => Some(CONNECTING),
            (None, _) => None,
        }
    }
}
