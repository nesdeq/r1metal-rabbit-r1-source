//! A translator session in its thread.

use super::{BlockKind, classify_block};
use crate::base::json::{number_at, text_at, unescape};
use crate::codec::aac;
use crate::domain::voice::Phase;
use crate::hw::sound::{fill, peak, to_span};
use crate::openai::fault::{Ending, Fault};
use crate::openai::models::interpreter;
use crate::openai::money::{dollars, parse_whole_seconds};
use crate::openai::refusal::parse_refusal;
use crate::openai::voice::Downlink;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

const PIECE_SAMPLES: usize = 2 * aac::FRAME;
/// The service's time to end a session, and to take each send while it stands.
pub const CLOSING_TIMEOUT: Duration = Duration::from_secs(3);
const NETWORK_GONE: &str = "the network is gone";
/// The log line of a session whose network went.
pub const GONE_LINE: &str = "translator: the network is gone";

/// The ending of a session whose network went.
pub fn to_network_gone() -> Ending {
    Ending::Failed(Fault::Offline(NETWORK_GONE.into()))
}

/// The ending of a session the screen let go of whose close the service could not be asked for or did not answer: no failure, and its seconds are those it lasted.
pub const UNCLOSED: Ending = Ending::Done;

/// The log line of a close the service did not answer within `CLOSING_TIMEOUT`.
pub fn to_unanswered_line() -> String {
    format!(
        "translator: the service did not answer the session's close in {} ms, and the seconds it lasted are paid for",
        CLOSING_TIMEOUT.as_millis()
    )
}

/// The log line of a session that stands.
pub fn to_stood_line(round: u32, ms: u128) -> String {
    format!("translator: session {round} stands after {ms} ms")
}

/// The log line of a session's cost and remarks.
pub fn to_session_line(round: u32, seconds: u64, micros: u64, remarks: usize) -> String {
    format!("translator: session {round}: {seconds} s for {}, {remarks} remarks", dollars(micros))
}

/// The log line of a stored conversation.
pub fn to_kept_line(stem: &str) -> String {
    format!("translator: the conversation is kept as {stem}")
}

/// The thread's action on a message of the service's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Nothing,
    PlayVoice,
    // More of what the model heard, with the millisecond of sound it begins at.
    AddSource(String, Option<u64>),
    // More of what the model said, with the milliseconds of sound sent.
    AddTranslation(String, u64),
    // Hands the model's delegated work back, so that it does not wait for an answer that never comes.
    HandBack(String),
    End(Ending),
}

#[derive(Default)]
struct Counts {
    sent_samples: u64,
    silenced_samples: u64,
    loudest_sample: u16,
    messages: u64,
    voice_samples: u64,
    max_queued_samples: usize,
    unhandled_kinds: Vec<(String, u32)>,
}

/// The speaker's counts of a session, for the log.
#[derive(Clone, Copy, Default)]
pub struct Playout {
    voice_samples: u64,
    empty_samples: u64,
    silence_peak: u32,
}

/// The sound queues and the speaker's counts.
#[derive(Default)]
pub struct Lanes {
    pub up: VecDeque<i16>,
    pub down: VecDeque<i16>,
    pub echo_until_ns: u64,
    pub playout: Playout,
}

impl Lanes {
    /// Fills a speaker block at `now_ns`, with `pending` samples before it.
    pub fn fill_block(&mut self, block: &mut [i16], pending: usize, now_ns: u64) {
        fill(block, &mut self.down);
        match classify_block(block, pending) {
            BlockKind::Voice(echo) => {
                self.echo_until_ns = now_ns + echo.as_nanos() as u64;
                self.playout.voice_samples += block.len() as u64;
            }
            BlockKind::Empty => self.playout.empty_samples += block.len() as u64,
            BlockKind::Silence(loudest) => {
                self.playout.silence_peak = self.playout.silence_peak.max(u32::from(loudest));
            }
        }
    }
}

/// A session that stands.
#[derive(Default)]
pub struct Session {
    piece: Vec<i16>,
    is_echoing: bool,
    was_echoing: bool,
    downlink: Downlink,
    paid_s: Option<u64>,
    close_start: Option<Duration>,
    is_closed: bool,
    counts: Counts,
    setup_time: Duration,
}

impl Session {
    pub fn new(setup_time: Duration) -> Session {
        Session { setup_time, ..Session::default() }
    }

    /// True once, when the service is to be asked to end the session.
    pub fn should_close(&mut self, is_wanted: bool, now: Duration) -> bool {
        let should = !is_wanted && self.close_start.is_none();
        if should {
            self.close_start = Some(now);
        }
        should
    }

    /// The longest wait from `now`, none without a limit.
    pub fn wait_limit(&self, now: Duration) -> Option<Duration> {
        Some((self.close_start? + CLOSING_TIMEOUT).saturating_sub(now))
    }

    /// Sets whether the model's voice sounds at `now_ns`.
    pub fn set_echo(&mut self, now_ns: u64, echo_until_ns: u64) {
        self.is_echoing = now_ns < echo_until_ns;
    }

    /// Takes the next piece to send, silent while the voice sounds; false while less than a piece waits.
    pub fn take_piece(&mut self, queue: &mut VecDeque<i16>) -> bool {
        if queue.len() < PIECE_SAMPLES {
            return false;
        }
        let is_silent = self.is_echoing;
        self.piece.clear();
        self.piece.extend(queue.drain(..PIECE_SAMPLES).map(|sample| if is_silent { 0 } else { sample }));
        true
    }

    /// The last piece to send; none once closing, when it is only counted.
    pub fn piece(&self) -> Option<&[i16]> {
        self.close_start.is_none().then_some(&self.piece)
    }

    /// Counts the last piece as sent.
    pub fn count_piece(&mut self) {
        let samples = self.piece.len() as u64;
        self.counts.sent_samples += samples;
        if self.is_echoing {
            self.counts.silenced_samples += samples;
        } else {
            self.counts.loudest_sample = self.counts.loudest_sample.max(peak(&self.piece));
        }
    }

    /// The new phase where the voice began or stopped sounding.
    pub fn take_phase_change(&mut self) -> Option<Phase> {
        let has_turned = self.was_echoing != self.is_echoing;
        self.was_echoing = self.is_echoing;
        if !has_turned || self.close_start.is_some() {
            return None;
        }
        Some(if self.is_echoing { Phase::Speaking } else { Phase::Listening })
    }

    fn take_voice(&mut self, base64: &str) -> Action {
        if !self.downlink.take(base64) {
            return Action::Nothing;
        }
        self.counts.voice_samples += self.downlink.model_samples();
        Action::PlayVoice
    }

    fn count_unhandled(&mut self, kind: &str) {
        match self.counts.unhandled_kinds.iter_mut().find(|entry| entry.0 == kind) {
            Some(entry) => entry.1 += 1,
            None => self.counts.unhandled_kinds.push((kind.to_string(), 1)),
        }
    }

    fn handle(&mut self, kind: &str, message: &str) -> Option<(Action, Option<String>)> {
        let delta = || text_at(message, &["delta"]).map(unescape).unwrap_or_default();
        let paid_s = || parse_whole_seconds(message, &["usage", "seconds"]);
        match kind {
            "session.started" => Some((Action::Nothing, None)),
            "session.output_audio.delta" => {
                Some((self.take_voice(text_at(message, &["delta"]).unwrap_or_default()), None))
            }
            "session.input_transcript.delta" => {
                Some((Action::AddSource(delta(), number_at(message, &["start_ms"])), None))
            }
            "session.output_transcript.delta" => {
                let sent = to_span(self.counts.sent_samples, interpreter::RATE);
                Some((Action::AddTranslation(delta(), sent.as_millis() as u64), None))
            }
            "session.usage.updated" => {
                self.paid_s = Some(paid_s()?);
                Some((Action::Nothing, None))
            }
            "session.closed" => {
                self.is_closed = true;
                self.paid_s = paid_s().or(self.paid_s);
                let reason = text_at(message, &["reason"]).map(unescape).unwrap_or_default();
                Some((Action::End(Ending::Done), Some(format!("translator: the service ended the session: {reason}"))))
            }
            "session.delegation.created" => {
                let work = unescape(text_at(message, &["delegation", "id"])?);
                let action = if self.close_start.is_none() { Action::HandBack(work) } else { Action::Nothing };
                Some((
                    action,
                    Some(String::from("translator: the model handed work over, and is told to interpret by itself")),
                ))
            }
            "error" => {
                let line = format!("translator: the service says {}", parse_refusal(message)?.to_log_line(None));
                Some((Action::Nothing, Some(line)))
            }
            _ => None,
        }
    }

    /// The action on a message, none meaning the service closed the socket, with its log line.
    pub fn take_message(&mut self, message: Option<&str>) -> (Action, Option<String>) {
        let Some(message) = message else {
            return (Action::End(Ending::Done), None);
        };
        self.counts.messages += 1;
        let kind = text_at(message, &["type"]).unwrap_or_default();
        self.handle(kind, message).unwrap_or_else(|| {
            self.count_unhandled(kind);
            (Action::Nothing, None)
        })
    }

    /// The log line of the message kinds nothing handled.
    pub fn to_unhandled_line(&self) -> Option<String> {
        let kinds: Vec<String> =
            self.counts.unhandled_kinds.iter().map(|(kind, times)| format!("{times} of {kind:?}")).collect();
        (!kinds.is_empty()).then(|| format!("translator: the service also sent {}", kinds.join(", ")))
    }

    /// Queues the last message's voice for the speaker.
    pub fn queue_voice(&mut self, lane: &mut VecDeque<i16>) {
        self.counts.max_queued_samples = self.counts.max_queued_samples.max(lane.len());
        lane.extend(self.downlink.voice());
    }

    /// The session's last log lines and the seconds to book.
    pub fn finish(&self, round: u32, duration: Duration, playout: Playout) -> ([String; 2], Option<u64>) {
        let counts = &self.counts;
        let model_s = |samples| to_span(samples, interpreter::RATE).as_secs();
        let card_span = |samples| to_span(samples, aac::RATE);
        let lines = [
            format!(
                "translator: session {round} over after {} s: {} s of the microphone went up, {} s of that as silence while the voice sounded, the largest sample of the rest {} of 32768",
                duration.as_secs(),
                model_s(counts.sent_samples),
                model_s(counts.silenced_samples),
                counts.loudest_sample
            ),
            format!(
                "translator: {} s of sound came down in {} messages; the speaker played {} s of the model's voice, {} s of nothing at all, and the rest no larger than {} of 32768; the most that waited to be played was {} ms",
                model_s(counts.voice_samples),
                counts.messages,
                card_span(playout.voice_samples).as_secs(),
                card_span(playout.empty_samples).as_secs(),
                playout.silence_peak,
                card_span(counts.max_queued_samples as u64).as_millis()
            ),
        ];
        let lasted_s = duration.saturating_sub(self.setup_time).as_secs();
        // A close the service left unanswered gave no last count: what it told before is no more than a floor.
        let is_unanswered = self.close_start.is_some() && !self.is_closed;
        let paid_s = match self.paid_s {
            Some(told_s) if is_unanswered => told_s.max(lasted_s),
            Some(told_s) => told_s,
            None => lasted_s,
        };
        (lines, (paid_s > 0).then_some(paid_s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    fn playout(voice_samples: u64, empty_samples: u64, silence_peak: u32) -> Playout {
        Playout { voice_samples, empty_samples, silence_peak }
    }

    #[test]
    fn lines_of_a_session() {
        assert_eq!(to_stood_line(3, 7), "translator: session 3 stands after 7 ms");
        assert_eq!(to_session_line(3, 60, 50_000, 9), "translator: session 3: 60 s for $0.05, 9 remarks");
        assert_eq!(to_kept_line("t"), "translator: the conversation is kept as t");
        assert_eq!(to_network_gone(), Ending::Failed(Fault::Offline("the network is gone".into())));
        assert_eq!(
            (UNCLOSED, to_unanswered_line().as_str()),
            (
                Ending::Done,
                "translator: the service did not answer the session's close in 3000 ms, and the seconds it lasted are paid for"
            ),
            "a session let go of that the service does not end is no failure"
        );
    }

    #[test]
    fn finish_lines() {
        let session = Session {
            counts: Counts {
                sent_samples: 90 * 24_000,
                silenced_samples: 30 * 24_000,
                loudest_sample: 9000,
                voice_samples: 41 * 24_000,
                messages: 812,
                max_queued_samples: 72_000,
                unhandled_kinds: Vec::new(),
            },
            paid_s: Some(94),
            ..Session::default()
        };
        assert_eq!(
            session.finish(3, Duration::from_secs(95), playout(33 * 48_000, 2 * 48_000, 12)),
            (
                [
                    "translator: session 3 over after 95 s: 90 s of the microphone went up, 30 s of that as silence while the voice sounded, the largest sample of the rest 9000 of 32768".into(),
                    "translator: 41 s of sound came down in 812 messages; the speaker played 33 s of the model's voice, 2 s of nothing at all, and the rest no larger than 12 of 32768; the most that waited to be played was 1500 ms".into()
                ],
                Some(94)
            )
        );
    }

    #[test]
    fn speaker_blocks() {
        use crate::hw::sound::PEAK_FLOOR;
        let (loud, second) = (PEAK_FLOOR as i16, aac::RATE as usize);
        let mut lanes = Lanes::default();
        lanes.down.extend([loud; 3]);
        let mut block = [9i16; 4];
        lanes.fill_block(&mut block, second - 4, 1_000);
        assert_eq!(block, [loud, loud, loud, 0], "filled from what waits, with silence past it");
        let echo_until_ns = 1_000 + (Duration::from_secs(1) + super::super::ECHO).as_nanos() as u64;
        assert_eq!(
            (lanes.echo_until_ns, lanes.playout.voice_samples),
            (echo_until_ns, 4),
            "the model's voice sounds until what waits before the block and the block are played, and a moment after"
        );
        lanes.fill_block(&mut block, 0, 2_000);
        assert_eq!((block, lanes.playout.empty_samples), ([0; 4], 4), "nothing waits: a block of nothing at all");
        lanes.down.extend([loud - 1, -3]);
        lanes.fill_block(&mut block, 0, 3_000);
        assert_eq!(
            (lanes.playout.silence_peak, lanes.playout.voice_samples, lanes.playout.empty_samples, lanes.echo_until_ns),
            (PEAK_FLOOR - 1, 4, 4, echo_until_ns),
            "the service's silence counts its peak alone, and moves the echo no further"
        );
    }

    #[test]
    fn close_once() {
        let mut session = Session::new(ms(900));
        assert_eq!(session.wait_limit(ms(10)), None);
        assert!(!session.should_close(true, ms(10)));
        assert!(session.should_close(false, ms(10)) && !session.should_close(false, ms(20)));
        assert_eq!(session.wait_limit(ms(1010)), Some(CLOSING_TIMEOUT - ms(1000)));
        assert_eq!(session.wait_limit(ms(10) + CLOSING_TIMEOUT * 2), Some(Duration::ZERO));
    }

    #[test]
    fn echo_silences_uplink() {
        let mut session = Session::new(Duration::ZERO);
        let mut queue: VecDeque<i16> = (0..PIECE_SAMPLES as i16 * 2 + 5).map(|at| at % 100 - 50).collect();
        session.set_echo(100, 50);
        assert!(session.take_piece(&mut queue));
        assert!(session.piece().unwrap().len() == PIECE_SAMPLES && session.piece().unwrap()[1] == -49);
        session.count_piece();
        assert_eq!(session.take_phase_change(), None);
        session.set_echo(100, 200);
        assert!(session.take_piece(&mut queue) && session.piece().unwrap().iter().all(|&at| at == 0));
        session.count_piece();
        assert_eq!((session.take_phase_change(), session.take_phase_change()), (Some(Phase::Speaking), None));
        assert!(!session.take_piece(&mut queue) && queue.len() == 5);
        session.set_echo(300, 200);
        assert_eq!(session.take_phase_change(), Some(Phase::Listening));
        session.should_close(false, ms(1));
        session.set_echo(300, 400);
        assert_eq!(session.take_phase_change(), None);
        assert!(session.take_piece(&mut VecDeque::from([1; PIECE_SAMPLES])) && session.piece().is_none());
        session.count_piece();
        assert_eq!(
            (session.counts.sent_samples, session.counts.silenced_samples, session.counts.loudest_sample),
            (3 * PIECE_SAMPLES as u64, 2 * PIECE_SAMPLES as u64, 50)
        );
    }

    #[test]
    fn message_actions_and_paid_seconds() {
        let mut session = Session::new(ms(900));
        let mut take = |message: &str| session.take_message(Some(message));
        assert_eq!(take(r#"{"type":"session.output_audio.delta","delta":"AAABAA=="}"#), (Action::PlayVoice, None));
        assert_eq!(take(r#"{"type":"session.output_audio.delta","delta":"not sound"}"#), (Action::Nothing, None));
        assert_eq!(
            take(r#"{"type":"session.input_transcript.delta","delta":"Wo ist \"der\"","start_ms":1200,"end_ms":2900}"#),
            (Action::AddSource("Wo ist \"der\"".into(), Some(1200)), None)
        );
        assert_eq!(take(r#"{"type":"session.usage.updated","usage":{"seconds":29.4}}"#), (Action::Nothing, None));
        assert_eq!(
            take(r#"{"type":"error","error":{"code":"invalid_value","message":"No."}}"#),
            (Action::Nothing, Some("translator: the service says invalid_value: No.".into()))
        );
        assert_eq!(
            take(r#"{"type":"session.delegation.created","delegation":{"id":"item_1"}}"#),
            (
                Action::HandBack("item_1".into()),
                Some("translator: the model handed work over, and is told to interpret by itself".into())
            )
        );
        assert_eq!(take(r#"{"type":"session.started"}"#), (Action::Nothing, None));
        for other in [
            r#"{"type":"session.instructions.appended"}"#,
            r#"{"type":"session.instructions.appended"}"#,
            r#"{"type":"session.delegation.created","delegation":{"target":"client"}}"#,
            "not json",
        ] {
            assert_eq!(take(other), (Action::Nothing, None));
        }
        let mut lane = VecDeque::from([9i16; 3]);
        session.queue_voice(&mut lane);
        assert_eq!(lane.len(), 3 + 4);
        assert_eq!(
            session.to_unhandled_line().as_deref(),
            Some(
                r#"translator: the service also sent 2 of "session.instructions.appended", 1 of "session.delegation.created", 1 of """#
            )
        );
        session.take_piece(&mut VecDeque::from([0; PIECE_SAMPLES]));
        session.count_piece();
        assert_eq!(
            session.take_message(Some(r#"{"type":"session.output_transcript.delta","delta":"Where is"}"#)),
            (Action::AddTranslation("Where is".into(), 85), None)
        );
        session.should_close(false, ms(1));
        let outcome =
            session.take_message(Some(r#"{"type":"session.delegation.created","delegation":{"id":"item_2"}}"#));
        assert_eq!(outcome.0, Action::Nothing);
        assert_eq!(
            session.take_message(Some(r#"{"type":"session.closed","reason":"expired"}"#)),
            (Action::End(Ending::Done), Some("translator: the service ended the session: expired".into()))
        );
        assert_eq!(session.take_message(None), (Action::End(Ending::Done), None));
        assert_eq!(session.finish(4, ms(61_900), playout(1, 2, 3)).1, Some(30));
        let mut closed = Session::new(ms(900));
        closed.take_message(Some(r#"{"type":"session.closed","reason":"close_requested","usage":{"seconds":61}}"#));
        assert_eq!(closed.finish(1, ms(5000), Playout::default()).1, Some(61));
        assert_eq!(Session::new(ms(900)).finish(1, ms(61_900), Playout::default()).1, Some(61));
        let mut unanswered = Session::new(ms(900));
        unanswered.take_message(Some(r#"{"type":"session.usage.updated","usage":{"seconds":29.4}}"#));
        unanswered.should_close(false, ms(60_000));
        assert_eq!(
            unanswered.finish(1, ms(61_900), Playout::default()).1,
            Some(61),
            "a close left unanswered is paid by the seconds the session lasted, not by an earlier count"
        );
        assert_eq!(Session::new(ms(900)).finish(1, ms(500), Playout::default()).1, None);
        assert_eq!(Session::new(ms(900)).to_unhandled_line(), None);
    }
}
