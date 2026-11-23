//! A text read aloud passage by passage.

use super::Script;
use super::pace::{LEAD_FACTOR, Pace};
use crate::base::json::{text_at, unescape};
use crate::base::text::words;
use crate::codec::aac;
use crate::hw::sound::{self, to_span};
use crate::openai::aloud;
use crate::openai::fault::{Ending, Fault};
use crate::openai::models::reader;
use crate::openai::money::dollars;
use crate::openai::refusal::parse_refusal;
use crate::openai::voice::Downlink;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

/// A passage's first voice takes 1.4 s at the most as measured; room for a service slower than that to answer a stop.
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const BURST: Duration = Duration::from_secs(5);
const STALL_TIMEOUT: Duration = BURST.saturating_mul(LEAD_FACTOR);
const LET_GO_LINE: &str = "aloud: the session is let go of while the reading is held";
/// The log line of a session the service ended.
pub const SESSION_ENDED_LINE: &str = "aloud: the service ended the session";
/// The log line of a session the service ended with a passage's cost owed.
pub const CLOSED_UNPAID_LINE: &str = "aloud: the service ended the session before it told what the passage cost";
/// The log line of a network gone with a passage's cost owed.
pub const GONE_UNPAID_LINE: &str = "aloud: the network is gone before the service told what the passage cost";

fn is_verbatim(passage: &str, spoken: &str) -> bool {
    words(passage).eq(words(spoken))
}

/// The log line of a network gone under a reading, by whether it was still wanted.
pub fn to_gone_line(is_wanted: bool) -> String {
    format!("aloud: the network is gone{}", if is_wanted { "" } else { ", and the screen has let the reading go" })
}

/// The log line of a session that stood after `ms`.
pub fn to_session_line(ms: u128) -> String {
    format!("aloud: a session stands after {ms} ms")
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct PassageDue {
    is_due: bool,
    wait: Duration,
}

impl PassageDue {
    const fn is_now(&self) -> bool {
        self.is_due && self.wait.is_zero()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SpeakerStep {
    End,
    Start,
    Keep,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Stage {
    Unwanted,
    #[default]
    Speaker,
    Link,
    Ask,
    Wait,
    Read,
    Pass,
}

/// What the reader's thread does next in a pass over the reading.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    End(Ending),
    Speak,
    // The session closes; the log line after that, where there is one.
    Close(Option<&'static str>),
    // A session opens, the voice that waits lasting that long.
    Connect(Duration),
    Ask,
    // Whatever wakes the thread, that long at the most.
    Wait(Option<Duration>),
    Read,
    // The pass is over.
    Pass,
}

/// What the reader's thread does next as it waits for the word on a passage on its way.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Settle<'a> {
    Done,
    // The service is told to stop that response.
    Stop(&'a str),
    Wait(Duration),
    // No word came in time; the log line.
    Late(String),
}

/// The wait for the word on a passage on its way as a reading ends.
pub struct SettleWait {
    deadline: Duration,
    limit: Duration,
    is_stopped: bool,
}

impl SettleWait {
    /// A wait begun at `now`, that long at the most.
    pub fn new(now: Duration, limit: Duration) -> SettleWait {
        SettleWait { deadline: now + limit, limit, is_stopped: false }
    }

    pub fn next_step<'a>(&mut self, reading: &'a Reading, now: Duration) -> Settle<'a> {
        if !reading.is_in_flight() {
            return Settle::Done;
        }
        if !self.is_stopped
            && let Some(response) = reading.response()
        {
            self.is_stopped = true;
            return Settle::Stop(response);
        }
        let time_left = self.deadline.saturating_sub(now);
        if time_left.is_zero() {
            let ms = self.limit.as_millis();
            return Settle::Late(format!("aloud: the service did not tell what the passage cost in {ms} ms"));
        }
        Settle::Wait(time_left)
    }
}

/// The voice on its way to the speaker, and what the speaker made of it.
#[derive(Default)]
pub struct Lane {
    voice: VecDeque<i16>,
    /// The speaker goes off.
    pub is_held: bool,
    is_last: bool,
    played_samples: u64,
    padded_blocks: u64,
    missing_samples: u64,
}

impl Lane {
    /// Fills a block with the waiting voice; false once the speaker is to go off.
    pub fn fill_block(&mut self, block: &mut [i16]) -> bool {
        if self.is_held || (self.voice.is_empty() && self.is_last) {
            return false;
        }
        let missing = sound::fill(block, &mut self.voice) as u64;
        self.played_samples += block.len() as u64 - missing;
        // The last of the voice padded to a block is no gap in the reading.
        if missing > 0 && !self.is_last {
            self.padded_blocks += 1;
            self.missing_samples += missing;
        }
        true
    }

    pub fn add_voice(&mut self, voice: &[i16]) {
        self.voice.extend(voice);
    }

    pub fn pending_samples(&self) -> usize {
        self.voice.len()
    }

    pub const fn played_samples(&self) -> u64 {
        self.played_samples
    }

    /// The blocks padded with silence, and the samples that lacked.
    pub const fn silence(&self) -> (u64, u64) {
        (self.padded_blocks, self.missing_samples)
    }
}

/// What a message of the service came to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MessageOutcome {
    Nothing,
    // The first voice of the passage came.
    Began,
    // The passage is done and counted.
    Done,
    Refused(String),
    // The service has ended the session.
    Closed,
}

impl MessageOutcome {
    /// How the reading ends on what the service said, where that ends it.
    pub fn into_ending(self) -> Option<Ending> {
        match self {
            MessageOutcome::Refused(reason) => Some(Ending::Failed(Fault::Refused(reason))),
            MessageOutcome::Closed => {
                Some(Ending::Failed(Fault::Temporary(String::from("the service ended the session"))))
            }
            MessageOutcome::Nothing | MessageOutcome::Began | MessageOutcome::Done => None,
        }
    }
}

#[derive(Default)]
struct InFlight {
    asked_at: Duration,
    has_begun: bool,
    response: Option<String>,
}

/// How a reading stands.
#[derive(Default)]
pub struct Reading {
    passages: Vec<String>,
    is_growing: bool,
    next_index: usize,
    in_flight: Option<InFlight>,
    last_word_at: Duration,
    first_voice_delay: Duration,
    pace: Pace,
    is_held: bool,
    // A reply being written has its session opened before its first passage comes, once.
    is_link_due: bool,
    retry_at: Option<Duration>,
    is_session_lost: bool,
    micros: u64,
    voiced_samples: u64,
    misread_passages: usize,
    queued_samples: u64,
    passage_ends: Vec<u64>,
    downlink: Downlink,
    stage: Stage,
    pending_samples: usize,
    ask_in: Duration,
    due: PassageDue,
}

impl Reading {
    /// A reading begun at `now`.
    pub fn new(passages: Vec<String>, is_growing: bool, now: Duration) -> Reading {
        Reading { passages, is_growing, is_link_due: is_growing, last_word_at: now, ..Reading::default() }
    }

    /// Begins a pass, `hold` telling whether a reading still wanted is held; the log line where the hold changed.
    pub fn begin_pass(&mut self, hold: Option<bool>, lane: &mut Lane) -> Option<&'static str> {
        let Some(is_held) = hold else {
            self.stage = Stage::Unwanted;
            return None;
        };
        let line = self.set_held(is_held);
        (lane.is_held, lane.is_last) = (self.is_held, self.is_done());
        (self.stage, self.pending_samples) = (Stage::Speaker, lane.pending_samples());
        line
    }

    fn voice_left(&self) -> Duration {
        to_span(self.pending_samples as u64, aac::RATE)
    }

    /// The next step of the pass at `now`, the speaker playing where `is_playing` and a session standing where `has_link`.
    pub fn next_step(&mut self, is_playing: bool, has_link: bool, now: Duration) -> Step {
        loop {
            match self.stage {
                Stage::Unwanted => return Step::End(Ending::Done),
                Stage::Speaker => {
                    self.stage = Stage::Link;
                    match self.speaker_step(self.pending_samples, is_playing) {
                        SpeakerStep::End => return Step::End(Ending::Done),
                        SpeakerStep::Start => return Step::Speak,
                        SpeakerStep::Keep => {}
                    }
                }
                Stage::Link => {
                    self.stage = Stage::Ask;
                    let (should_keep_link, ask_in) = self.plan_wait(has_link, self.voice_left());
                    self.ask_in = ask_in;
                    if !should_keep_link && has_link {
                        return Step::Close(self.is_held.then_some(LET_GO_LINE));
                    }
                }
                Stage::Ask => {
                    self.due = self.next_due(self.ask_in, now);
                    let is_first_link = core::mem::take(&mut self.is_link_due);
                    if (is_first_link || self.due.is_now()) && !has_link {
                        return Step::Connect(self.voice_left());
                    }
                    self.stage = Stage::Wait;
                    if self.due.is_now() {
                        return Step::Ask;
                    }
                }
                Stage::Wait => {
                    self.stage = Stage::Read;
                    return Step::Wait(self.wait_limit(now, self.due));
                }
                Stage::Read => {
                    self.stage = Stage::Pass;
                    if has_link {
                        return Step::Read;
                    }
                }
                Stage::Pass => return Step::Pass,
            }
        }
    }

    pub(super) fn grow(&mut self, script: &Script) {
        let count = self.passages.len();
        self.passages.extend(script.passages.iter().skip(count).cloned());
        self.is_growing = script.is_growing;
    }

    fn set_held(&mut self, is_held: bool) -> Option<&'static str> {
        let has_changed = self.is_held != is_held;
        self.is_held = is_held;
        has_changed.then_some(if is_held { "aloud: held" } else { "aloud: goes on" })
    }

    fn is_done(&self) -> bool {
        self.in_flight.is_none() && !self.has_more() && !self.is_growing
    }

    /// True while a passage is on its way.
    pub fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// How many passages were heard to their end.
    pub fn heard_count(&self, played_samples: u64) -> usize {
        self.passage_ends.iter().filter(|end| **end <= played_samples).count()
    }

    pub fn next_passage(&self) -> (usize, &str) {
        (self.next_index, &self.passages[self.next_index])
    }

    /// Notes that the next passage was asked for at `now`.
    pub fn mark_asked(&mut self, now: Duration) {
        self.in_flight = Some(InFlight { asked_at: now, ..InFlight::default() });
        self.last_word_at = now;
    }

    fn response(&self) -> Option<&str> {
        self.in_flight.as_ref()?.response.as_deref()
    }

    /// The session could not ask for the next passage; the log line.
    pub fn fail_asking(&mut self, reason: impl core::fmt::Display) -> String {
        self.is_session_lost = true;
        format!("aloud: asking for passage {} of {}: {reason}", self.next_index + 1, self.passages.len())
    }

    pub fn lose_session(&mut self) {
        self.is_session_lost = true;
    }

    /// How the reading ends where the network goes; a sleep's stay ending under it is no failure.
    pub fn end_offline(&mut self, is_wanted: bool) -> Ending {
        if !is_wanted {
            return Ending::Done;
        }
        self.is_session_lost = true;
        Ending::Failed(Fault::Offline(String::from("the network is gone")))
    }

    /// True where what the service made of the passage on its way is waited for as the reading ends.
    pub fn should_settle(&self) -> bool {
        !self.is_session_lost && self.is_in_flight()
    }

    fn stall_left(&self, now: Duration) -> Option<Duration> {
        self.is_in_flight().then(|| (self.last_word_at + STALL_TIMEOUT).saturating_sub(now))
    }

    /// How the reading ends once the passage on its way has gone too long without a word, with the log line.
    pub fn check_stall(&self, now: Duration) -> Option<(Ending, String)> {
        self.stall_left(now).filter(Duration::is_zero).map(|_| {
            let line = format!(
                "aloud: the service said nothing of passage {} of {} for {} ms",
                self.next_index + 1,
                self.passages.len(),
                STALL_TIMEOUT.as_millis()
            );
            (Ending::Timeout, line)
        })
    }

    fn has_more(&self) -> bool {
        self.next_index < self.passages.len()
    }

    fn speaker_step(&self, pending_samples: usize, is_playing: bool) -> SpeakerStep {
        if is_playing {
            SpeakerStep::Keep
        } else if self.is_done() && pending_samples == 0 {
            SpeakerStep::End
        } else if !self.is_held && pending_samples > 0 {
            SpeakerStep::Start
        } else {
            SpeakerStep::Keep
        }
    }

    // As `Pace::plan_wait`, the session kept too while a passage is on its way.
    fn plan_wait(&self, has_link: bool, voice_left: Duration) -> (bool, Duration) {
        let has_more = self.has_more() || self.is_growing;
        let (should_keep_link, ask_in) = self.pace.plan_wait(has_link, voice_left, has_more, self.is_held);
        (should_keep_link || self.is_in_flight(), ask_in)
    }

    fn next_due(&self, ask_in: Duration, now: Duration) -> PassageDue {
        let retry_wait = self.retry_at.map_or(Duration::ZERO, |at| at.saturating_sub(now));
        PassageDue { is_due: !self.is_held && !self.is_in_flight() && self.has_more(), wait: ask_in.max(retry_wait) }
    }

    /// Notes a session that took `duration` to open; the pass ends, so that the next asks whether the reading is still wanted before a passage is paid for.
    pub fn mark_connected(&mut self, duration: Duration) {
        self.pace.add_reconnect(duration);
        self.retry_at = None;
        self.stage = Stage::Pass;
    }

    /// A session could not be opened; none where it is tried again, in the next pass, else how the reading ends.
    pub fn fail_connecting(&mut self, fault: Fault, voice_left: Duration, now: Duration) -> Option<Ending> {
        self.stage = Stage::Pass;
        if voice_left.is_zero() || fault.is_permanent() {
            return Some(Ending::Failed(fault));
        }
        self.retry_at = Some(now + self.pace.reconnect());
        None
    }

    fn wait_limit(&self, now: Duration, due: PassageDue) -> Option<Duration> {
        self.stall_left(now).or(due.is_due.then_some(due.wait))
    }

    /// Takes what a message said at `now`, none once the service ended the session, with the log line; its voice is taken where `takes_voice`, which a reading let go of that settles does not.
    pub fn take_message(
        &mut self,
        message: Option<&str>,
        now: Duration,
        takes_voice: bool,
    ) -> (MessageOutcome, Option<String>) {
        self.last_word_at = now;
        self.downlink.clear_voice();
        let Some(message) = message else {
            self.is_session_lost = true;
            return (MessageOutcome::Closed, None);
        };
        match text_at(message, &["type"]).unwrap_or_default() {
            "response.created" => {
                let name = text_at(message, &["response", "id"]);
                if let Some((flight, name)) = self.in_flight.as_mut().zip(name) {
                    flight.response = Some(name.to_string());
                }
                (MessageOutcome::Nothing, None)
            }
            "response.output_audio.delta" if takes_voice => {
                self.take_voice(text_at(message, &["delta"]).unwrap_or_default(), now)
            }
            "response.output_audio_transcript.done" if self.is_in_flight() => {
                (MessageOutcome::Nothing, self.check_verbatim(text_at(message, &["transcript"])))
            }
            "response.done" => self.take_done(message),
            "error" => match parse_refusal(message) {
                Some(refusal) => {
                    self.is_session_lost = true;
                    let text = refusal.to_log_line(None);
                    let line = format!("aloud: the service says {text}");
                    (MessageOutcome::Refused(text), Some(line))
                }
                None => (MessageOutcome::Nothing, None),
            },
            _ => (MessageOutcome::Nothing, None),
        }
    }

    fn check_verbatim(&mut self, transcript: Option<&str>) -> Option<String> {
        let spoken = transcript.map(unescape)?;
        let index = self.next_index.min(self.passages.len().saturating_sub(1));
        let passage = &self.passages[index];
        if is_verbatim(passage, &spoken) {
            return None;
        }
        self.misread_passages += 1;
        // The words are the owner's or a contact's: the log tells how many, never which.
        Some(format!(
            "aloud: passage {} of {} was said otherwise: {} words for its {}",
            self.next_index + 1,
            self.passages.len(),
            words(&spoken).count(),
            words(passage).count()
        ))
    }

    fn take_voice(&mut self, piece: &str, now: Duration) -> (MessageOutcome, Option<String>) {
        if !self.downlink.take(piece) {
            return (MessageOutcome::Nothing, None);
        }
        self.voiced_samples += self.downlink.model_samples();
        self.queued_samples += self.downlink.voice().len() as u64;
        let Some(flight) = self.in_flight.as_mut().filter(|flight| !flight.has_begun) else {
            return (MessageOutcome::Nothing, None);
        };
        flight.has_begun = true;
        self.first_voice_delay = now.saturating_sub(flight.asked_at);
        self.pace.add_first_voice(self.first_voice_delay);
        (MessageOutcome::Began, None)
    }

    fn take_done(&mut self, message: &str) -> (MessageOutcome, Option<String>) {
        let Some(flight) = self.in_flight.take() else {
            return (MessageOutcome::Nothing, None);
        };
        let (micros, usage) = aloud::parse_done(message);
        self.micros = self.micros.saturating_add(micros);
        let index = self.next_index.min(self.passages.len().saturating_sub(1));
        let characters = self.passages[index].chars().count();
        let first_voice = if flight.has_begun {
            format!("its voice began after {} ms", self.first_voice_delay.as_millis())
        } else {
            String::from("none of its voice was played")
        };
        let line = format!(
            "aloud: passage {} of {}: {characters} characters, {first_voice}, {usage}",
            self.next_index + 1,
            self.passages.len()
        );
        self.next_index += 1;
        self.passage_ends.push(self.queued_samples);
        (MessageOutcome::Done, Some(line))
    }

    /// The voice the last message brought, at the card's rate.
    pub fn voice(&self) -> &[i16] {
        self.downlink.voice()
    }

    /// The log's last line and what the reading is booked at; none where no passage was paid for.
    pub fn finish(
        &self,
        round: u32,
        duration: Duration,
        (padded, missing): (u64, u64),
        sessions: &SessionStats,
    ) -> (String, Option<u64>) {
        let line = format!(
            "aloud: reading {round} over after {} s: {} of {} passages paid for, {} said otherwise than written, {} s of voice came for {}, {padded} blocks, {} ms, of padded silence where the lane had no voice, the network held {} s in {} stays",
            duration.as_secs(),
            self.next_index,
            self.passages.len(),
            self.misread_passages,
            to_span(self.voiced_samples, reader::RATE).as_secs(),
            dollars(self.micros),
            to_span(missing, aac::RATE).as_millis(),
            sessions.total.as_secs(),
            sessions.count
        );
        (line, (self.next_index > 0).then_some(self.micros))
    }
}

/// How often a reading had a session, and for how long all of them stood.
#[derive(Default)]
pub struct SessionStats {
    count: u32,
    total: Duration,
}

impl SessionStats {
    /// True where the session to come is not the reading's first: the clocks are not checked again.
    pub const fn is_repeat(&self) -> bool {
        self.count > 0
    }

    pub const fn note_opened(&mut self) {
        self.count += 1;
    }

    pub fn note_ended(&mut self, duration: Duration) {
        self.total += duration;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    fn reading_of(passages: &[&str]) -> Reading {
        Reading::new(passages.iter().map(|passage| String::from(*passage)).collect(), false, Duration::ZERO)
    }

    #[test]
    fn script_growth() {
        let (mut reading, mut script) =
            (Reading::new(Vec::new(), true, Duration::ZERO), Script::for_reply("Say it.".into()));
        reading.grow(&script);
        assert!(reading.is_growing && reading.passages.is_empty());
        script.passages.extend([String::from("One."), String::from("Two.")]);
        reading.grow(&script);
        assert!(reading.is_growing && reading.passages == ["One.", "Two."]);
        script.passages.push(String::from("Three."));
        script.is_growing = false;
        reading.grow(&script);
        assert!(!reading.is_growing && reading.passages.len() == 3, "closed, no more come");
    }

    #[test]
    fn verbatim_check() {
        assert!(is_verbatim("Regen ab Mittag, 12 Grad.", "regen ab Mittag 12 Grad!"));
        assert!(is_verbatim("Größer, nicht?", "größer nicht"));
        assert!(!is_verbatim("Regen ab Mittag.", "Ab Mittag regnet es."));
        assert!(!is_verbatim("Regen ab Mittag.", "Regen ab Mittag. Nimm den Schirm."));
        assert!(!is_verbatim("12 Grad.", "zwölf Grad."));
    }

    #[test]
    fn block_fill_gaps() {
        let mut lane = Lane { is_held: true, ..Lane::default() };
        lane.add_voice(&[1, 2]);
        let mut block = [9i16; 4];
        assert!(!lane.fill_block(&mut block), "held, the speaker goes off");
        lane.is_held = false;
        assert!(lane.fill_block(&mut block));
        assert_eq!((block, lane.played_samples(), lane.silence()), ([1, 2, 0, 0], 2, (1, 2)));
        lane.add_voice(&[1]);
        lane.is_last = true;
        assert!(lane.fill_block(&mut block));
        assert_eq!(
            (lane.played_samples(), lane.silence()),
            (3, (1, 2)),
            "the last of the voice padded to a block is no gap"
        );
        assert!(!lane.fill_block(&mut block), "nothing left of the last voice: the speaker goes off");
    }

    #[test]
    fn asking_failure() {
        let mut reading = reading_of(&["Eins.", "Zwei."]);
        reading.mark_asked(ms(0));
        assert!(reading.should_settle());
        assert_eq!(reading.fail_asking("the socket broke"), "aloud: asking for passage 1 of 2: the socket broke");
        assert!(!reading.should_settle(), "the session is lost: nothing of it is waited for");
    }

    #[test]
    fn passage_stages() {
        let mut reading = reading_of(&["Eins.", "Zwei."]);
        assert_eq!(reading.next_passage(), (0, "Eins."));
        assert!(reading.next_due(ms(0), ms(0)).is_now());
        reading.mark_asked(ms(100));
        assert!(reading.is_in_flight() && !reading.next_due(ms(0), ms(0)).is_now());
        reading.take_message(Some(r#"{"type":"response.created","response":{"id":"r1"}}"#), ms(150), true);
        assert_eq!(reading.response(), Some("r1"));
        let (outcome, _) =
            reading.take_message(Some(r#"{"type":"response.output_audio.delta","delta":"AAABAA=="}"#), ms(900), true);
        assert_eq!(outcome, MessageOutcome::Began);
        assert_eq!(reading.voice().len(), 4);
        let (repeat, _) =
            reading.take_message(Some(r#"{"type":"response.output_audio.delta","delta":"AAABAA=="}"#), ms(950), true);
        assert_eq!(repeat, MessageOutcome::Nothing);
        let (done, line) = reading.take_message(Some(r#"{"type":"response.done","response":{}}"#), ms(2000), true);
        assert_eq!(done, MessageOutcome::Done);
        assert!(line.unwrap().starts_with("aloud: passage 1 of 2: 5 characters, its voice began after 800 ms, "));
        assert_eq!((reading.next_passage(), reading.is_done()), ((1, "Zwei."), false));
        assert_eq!(reading.heard_count(3), 0);
        assert_eq!(reading.heard_count(8), 1);
    }

    #[test]
    fn misread_line() {
        let mut reading = reading_of(&["Eins."]);
        reading.mark_asked(ms(0));
        let (_, line) = reading.take_message(
            Some(r#"{"type":"response.output_audio_transcript.done","transcript":"Zwei, drei."}"#),
            ms(1),
            true,
        );
        assert_eq!(
            line.as_deref(),
            Some("aloud: passage 1 of 1 was said otherwise: 2 words for its 1"),
            "the line counts the words and tells none of them"
        );
    }

    #[test]
    fn refusal_and_close_end() {
        let mut reading = reading_of(&["Eins."]);
        let (refusal, line) =
            reading.take_message(Some(r#"{"type":"error","error":{"code":"x","message":"No."}}"#), ms(1), true);
        assert_eq!(line.as_deref(), Some("aloud: the service says x: No."));
        assert_eq!(refusal.into_ending(), Some(Ending::Failed(Fault::Refused("x: No.".into()))));
        assert_eq!(
            reading.take_message(None, ms(2), true).0.into_ending(),
            Some(Ending::Failed(Fault::Temporary("the service ended the session".into())))
        );
        assert_eq!(MessageOutcome::Began.into_ending(), None);
    }

    #[test]
    fn speaker_steps() {
        let mut reading = reading_of(&["Eins."]);
        let table = [
            ((3, false, false), SpeakerStep::Start),
            ((3, true, false), SpeakerStep::Keep),
            ((0, false, false), SpeakerStep::Keep),
        ];
        for ((pending_samples, is_playing, is_held), step) in table {
            reading.is_held = is_held;
            assert_eq!(reading.speaker_step(pending_samples, is_playing), step);
        }
        reading.is_held = true;
        assert_eq!(reading.speaker_step(3, false), SpeakerStep::Keep);
        assert_eq!(reading_of(&[]).speaker_step(0, false), SpeakerStep::End);
    }

    #[test]
    fn stall_after_ten_seconds() {
        let mut reading = reading_of(&["Eins."]);
        reading.mark_asked(ms(0));
        assert!(reading.plan_wait(true, ms(60_000)).0, "the session is kept while a passage is on its way");
        assert_eq!(reading.check_stall(ms(9_999)), None);
        let (ending, line) = reading.check_stall(ms(10_000)).unwrap();
        assert_eq!(ending, Ending::Timeout);
        assert_eq!(line, "aloud: the service said nothing of passage 1 of 1 for 10000 ms");
    }

    #[test]
    fn reconnect_retry() {
        let fault = || Fault::Temporary("gone".into());
        let mut reading = reading_of(&["Eins."]);
        assert_eq!(reading.fail_connecting(fault(), ms(1000), ms(0)), None);
        assert_eq!(reading.next_due(ms(0), ms(0)).wait, ms(2424));
        reading.mark_connected(ms(100));
        assert_eq!(reading.next_due(ms(0), ms(0)).wait, ms(0));
        assert_eq!(reading.fail_connecting(fault(), ms(0), ms(0)), Some(Ending::Failed(fault())));
        assert_eq!(reading.fail_connecting(Fault::NoKey, ms(1000), ms(0)), Some(Ending::Failed(Fault::NoKey)));
    }

    #[test]
    fn offline_stop() {
        let mut reading = reading_of(&[]);
        assert_eq!(reading.end_offline(false), Ending::Done);
        assert!(!reading.should_settle());
        assert_eq!(reading.end_offline(true), Ending::Failed(Fault::Offline("the network is gone".into())));
    }

    #[test]
    fn unpaid_finish() {
        let sessions = SessionStats { count: 2, total: Duration::from_secs(30) };
        let (line, booked) = reading_of(&["Eins."]).finish(4, Duration::from_secs(7), (2, 480), &sessions);
        assert_eq!(booked, None);
        assert_eq!(
            line,
            alloc::format!(
                "aloud: reading 4 over after 7 s: 0 of 1 passages paid for, 0 said otherwise than written, 0 s of voice came for {}, 2 blocks, 10 ms, of padded silence where the lane had no voice, the network held 30 s in 2 stays",
                dollars(0)
            )
        );
        assert!(sessions.is_repeat() && !SessionStats::default().is_repeat());
    }

    #[test]
    fn hold_lines() {
        let mut reading = reading_of(&[]);
        assert_eq!(
            (reading.set_held(true), reading.set_held(true), reading.set_held(false)),
            (Some("aloud: held"), None, Some("aloud: goes on"))
        );
    }

    fn pass(reading: &mut Reading, pending_samples: usize) -> Lane {
        let mut lane = Lane::default();
        lane.add_voice(&vec![0; pending_samples]);
        reading.begin_pass(Some(false), &mut lane);
        lane
    }

    #[test]
    fn pass_order() {
        let mut reading = reading_of(&["Eins."]);
        pass(&mut reading, 0);
        assert_eq!(reading.next_step(false, false, ms(0)), Step::Connect(ms(0)));
        reading.mark_connected(ms(100));
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Pass, "a session opened ends the pass");
        let mut let_go = reading_of(&["Eins."]);
        pass(&mut let_go, 0);
        let_go.next_step(false, false, ms(0));
        let_go.mark_connected(ms(100));
        let_go.begin_pass(None, &mut Lane::default());
        assert_eq!(
            let_go.next_step(false, true, ms(0)),
            Step::End(Ending::Done),
            "a reading let go of while its session opened asks for no passage"
        );
        pass(&mut reading, 0);
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Ask);
        reading.mark_asked(ms(0));
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Wait(Some(STALL_TIMEOUT)));
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Read);
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Pass);
        assert_eq!(reading.next_step(false, true, ms(0)), Step::Pass);
    }

    #[test]
    fn pass_speaker() {
        let mut reading = reading_of(&["Eins."]);
        let lane = pass(&mut reading, 480_000);
        assert_eq!(lane.pending_samples(), 480_000);
        assert_eq!(reading.next_step(false, false, ms(0)), Step::Speak);
        assert_eq!(reading.next_step(true, false, ms(0)), Step::Wait(Some(ms(4776))));
        assert_eq!(reading.next_step(true, false, ms(0)), Step::Pass, "no session, nothing to read");
        let mut done = reading_of(&[]);
        pass(&mut done, 0);
        assert_eq!(done.next_step(false, false, ms(0)), Step::End(Ending::Done));
        let mut unwanted = reading_of(&["Eins."]);
        unwanted.begin_pass(None, &mut Lane::default());
        assert_eq!(unwanted.next_step(false, true, ms(0)), Step::End(Ending::Done));
    }

    #[test]
    fn pass_closes_link() {
        let mut reading = reading_of(&[]);
        reading.is_growing = true;
        pass(&mut reading, 0);
        reading.is_held = true;
        assert_eq!(reading.next_step(true, true, ms(0)), Step::Close(Some(LET_GO_LINE)));
        assert_eq!(reading.next_step(true, false, ms(0)), Step::Wait(None));
    }

    #[test]
    fn failed_connect() {
        let mut reading = reading_of(&["Eins."]);
        pass(&mut reading, 0);
        assert_eq!(reading.next_step(false, false, ms(0)), Step::Connect(ms(0)));
        assert_eq!(
            reading.fail_connecting(Fault::Temporary("gone".into()), ms(1), ms(0)),
            None,
            "voice is left, so it is tried again"
        );
        assert_eq!(reading.next_step(false, false, ms(0)), Step::Pass);
    }

    #[test]
    fn reply_links_first() {
        let fault = || Fault::Temporary("gone".into());
        let mut reply = Reading::new(Vec::new(), true, Duration::ZERO);
        pass(&mut reply, 0);
        assert_eq!(
            reply.next_step(false, false, ms(0)),
            Step::Connect(ms(0)),
            "a reply being written has its session opened before its first passage comes"
        );
        assert_eq!(
            reply.fail_connecting(fault(), ms(0), ms(0)),
            Some(Ending::Failed(fault())),
            "with no voice to play meanwhile, a session that cannot be opened ends the reading"
        );
        let mut reply = Reading::new(Vec::new(), true, Duration::ZERO);
        pass(&mut reply, 0);
        reply.next_step(false, false, ms(0));
        reply.mark_connected(ms(100));
        assert_eq!(reply.next_step(false, true, ms(0)), Step::Pass, "the session opened ends the pass");
        pass(&mut reply, 0);
        assert!(
            !matches!(reply.next_step(false, false, ms(0)), Step::Connect(_)),
            "opened ahead once: a session lost before any passage came is opened again as a passage is due"
        );
        let mut let_go = Reading::new(Vec::new(), true, Duration::ZERO);
        let_go.begin_pass(None, &mut Lane::default());
        assert_eq!(
            let_go.next_step(false, false, ms(0)),
            Step::End(Ending::Done),
            "a reply let go of before its first pass has no session opened"
        );
    }

    #[test]
    fn settle_steps() {
        let mut reading = reading_of(&["Eins."]);
        let mut wait = SettleWait::new(ms(10), ms(100));
        assert_eq!(wait.next_step(&reading, ms(10)), Settle::Done);
        reading.mark_asked(ms(0));
        assert_eq!(wait.next_step(&reading, ms(10)), Settle::Wait(ms(100)), "nothing to stop yet");
        reading.take_message(Some(r#"{"type":"response.created","response":{"id":"r1"}}"#), ms(20), true);
        assert_eq!(wait.next_step(&reading, ms(20)), Settle::Stop("r1"));
        assert_eq!(wait.next_step(&reading, ms(30)), Settle::Wait(ms(80)));
        assert_eq!(
            wait.next_step(&reading, ms(110)),
            Settle::Late("aloud: the service did not tell what the passage cost in 100 ms".into())
        );
    }

    #[test]
    fn gone_and_session_lines() {
        assert_eq!(to_gone_line(true), "aloud: the network is gone");
        assert_eq!(to_gone_line(false), "aloud: the network is gone, and the screen has let the reading go");
        assert_eq!(to_session_line(12), "aloud: a session stands after 12 ms");
    }
}
