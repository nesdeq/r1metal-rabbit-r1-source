//! A recording's words from the speech model: the request, its answer complete or streamed, and its cost.

use super::ledger::{Entry, Kind, Ledger};
use super::models::transcriber::MODEL;
use super::money::{cost_by_minute, parse_whole_seconds};
use super::refusal::{Refusal, parse_document, parse_refusal};
use crate::base::json::text_at;
use crate::base::json::unescape;
use crate::codec::aac;
use crate::codec::form::Form;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const MICROS_PER_MINUTE: u64 = 4500;
pub(crate) const MAX_BYTES: u64 = 25_000_000;
const MIN_SAMPLES: usize = aac::RATE as usize / 10;
const FILE_NAME: &str = "note.m4a";
const FILE_KIND: &str = "audio/mp4";
const FORM_SLACK: usize = 512;
const DURATION: &str = "duration";
const DELTA: &str = "transcript.text.delta";
const DONE: &str = "transcript.text.done";

/// What a recording written down is of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Subject {
    // A message said to the robot.
    Conversation,
    // A note's sound, whole or a piece of it.
    Note,
    // A message said for Signal.
    Message,
    // A name said to the Pokédex.
    PokedexName,
    // A request said to the browser.
    WebRequest,
    // A request said to the bell.
    BellRequest,
    // A request said for the rundowns.
    RundownsRequest,
}

impl Subject {
    const fn to_name(self) -> &'static str {
        match self {
            Subject::Conversation => "the conversation's sound",
            Subject::Note => "sound",
            Subject::Message => "a message said",
            Subject::PokedexName => "a name said to the Pokédex",
            Subject::WebRequest => "a request said to the browser",
            Subject::BellRequest => "a request said to the bell",
            Subject::RundownsRequest => "a request said for the rundowns",
        }
    }

    /// Books `seconds` of it, written down for `micros` millionths of a dollar, as its kind is booked.
    pub fn book(self, ledger: &mut Ledger, (year, month): (u32, u8), seconds: u64, micros: u64) {
        let entry = match self {
            Subject::Note => return ledger.add_sound(year, month, seconds, micros),
            Subject::Conversation => Entry::work(Kind::Chat),
            Subject::Message => Entry::one(Kind::Dictation),
            Subject::PokedexName => Entry::one(Kind::Pokedex),
            Subject::WebRequest => Entry::work(Kind::Web),
            Subject::BellRequest => Entry::work(Kind::Bell),
            Subject::RundownsRequest => Entry::work(Kind::Rundown),
        };
        ledger.add(year, month, entry, micros);
    }
}

/// The seconds of a recording of that many packets, a second begun counted whole, where it holds sound enough to be written down.
pub fn recording_seconds(packets: usize) -> Option<u64> {
    ((packets * aac::FRAME).saturating_sub(aac::DELAY) >= MIN_SAMPLES)
        .then(|| aac::seconds(packets, aac::DELAY, aac::RATE))
}

/// The request's body: a form with the model, the prompt where there is one, and the recording, an MP4 file of AAC sound; for an answer that streams where `is_streamed`.
pub fn request_form(boundary: &str, recording: &[u8], prompt: &str, is_streamed: bool) -> Vec<u8> {
    let form = Form::new(boundary, recording.len() + prompt.len() + FORM_SLACK).text("model", MODEL);
    let form = if is_streamed { form.text("stream", "true") } else { form };
    let form = if prompt.trim().is_empty() { form } else { form.text("prompt", prompt.trim()) };
    form.file("file", FILE_NAME, FILE_KIND, recording).end()
}

/// A streamed answer's end, once it has come.
#[derive(Default)]
pub struct Stream {
    end: Option<Result<Transcript, Error>>,
}

impl Stream {
    /// Takes the data of an event and returns more of the words to show; their end, or the service's refusal, is stored.
    pub fn take(&mut self, data: &str) -> Option<String> {
        match text_at(data, &["type"]) {
            Some(DELTA) => return Some(text_at(data, &["delta"]).map(unescape).unwrap_or_default()),
            Some(DONE) => self.end = Some(parse_answer(data.as_bytes())),
            _ => {
                if let Some(refusal) = parse_refusal(data) {
                    self.end = Some(Err(Error::Refused(refusal)));
                }
            }
        }
        None
    }

    /// True once the words' end has come.
    pub const fn has_end(&self) -> bool {
        self.end.is_some()
    }

    /// The transcript once the stream of that status and `body` has ended, or its fault.
    pub fn finish(self, status: u16, body: &[u8]) -> Result<Transcript, Fault> {
        self.end.unwrap_or_else(|| parse_answer(body)).map_err(|error| error.to_fault(status))
    }
}

/// A recording's words, and the seconds of sound the service counted, if it counts those.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Transcript {
    pub words: String,
    seconds: Option<u64>,
}

impl Transcript {
    /// The seconds paid for: those the service counted, else `duration`.
    pub fn paid_seconds(&self, duration: u64) -> u64 {
        self.seconds.unwrap_or(duration)
    }

    /// The log line of `bytes` of a recording of `subject` written down in `ms` milliseconds, `paid` seconds paid for.
    pub fn to_log_line(&self, bytes: usize, subject: Subject, ms: u128, paid: u64) -> String {
        format!(
            "openai: {bytes} bytes of {} written down as {} characters in {ms} ms, {paid} s paid for",
            subject.to_name(),
            self.words.chars().count()
        )
    }
}

/// Why an answer holds no words.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Refused(Refusal),
    NoWords,
}

impl Error {
    pub fn to_fault(self, status: u16) -> Fault {
        match self {
            Error::Refused(refusal) => Fault::from_refusal(Some(status), &refusal),
            Error::NoWords => Fault::Temporary(format!("status {status}, no words in the answer")),
        }
    }
}

/// Parses an answer of the service; a recording in which nothing was said has no words and is an answer all the same.
pub fn parse_answer(body: &[u8]) -> Result<Transcript, Error> {
    let document = parse_document(body).map_err(|refusal| refusal.map_or(Error::NoWords, Error::Refused))?;
    let words = text_at(document, &["text"]).map(unescape).ok_or(Error::NoWords)?;
    let is_by_duration = text_at(document, &["usage", "type"]) == Some(DURATION);
    let seconds = parse_whole_seconds(document, &["usage", "seconds"]).filter(|_| is_by_duration);
    Ok(Transcript { words: words.trim().into(), seconds })
}

/// The cost of that many seconds of sound in millionths of a dollar; more seconds than any recording has cost the most there is.
pub const fn sound_micros(seconds: u64) -> u64 {
    cost_by_minute(seconds, MICROS_PER_MINUTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::time::SECONDS_PER_MINUTE;
    use crate::openai::ledger::Paid;

    #[test]
    fn request_form_parts() {
        let body = request_form("XYZ", &[0, 0, 0, 28, b'f', b't', b'y', b'p'], "", false);
        assert!(body.starts_with(
            b"--XYZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-transcribe\r\n--XYZ\r\n"
        ));
        assert!(body.ends_with(
            b"name=\"file\"; filename=\"note.m4a\"\r\nContent-Type: audio/mp4\r\n\r\n\x00\x00\x00\x1Cftyp\r\n--XYZ--\r\n"
        ));
        let text = String::from_utf8_lossy(&request_form("XYZ", b"m4a", " Pikachu ", false)).into_owned();
        assert!(
            text.contains("name=\"prompt\"\r\n\r\nPikachu\r\n") && !text.contains("stream"),
            "what the model is told, without the words streamed: {text}"
        );
        assert_eq!(
            [recording_seconds(0), recording_seconds(5), recording_seconds(6)],
            [None, None, Some(1)],
            "five packets past the lead are 4096 samples, under a tenth of a second at 48 kHz, and six 5120, a second begun"
        );
        assert_eq!(recording_seconds(48), Some(2), "47 packets past the lead are 48128 samples, past a second");
    }

    #[test]
    fn answers() {
        let body = br#"{"text": " Bonjour, \"pouvez\"-vous m'entendre ?\n", "languages": [{"code": "fr"}], "usage": {"type": "duration", "seconds": 27}}"#;
        assert_eq!(
            parse_answer(body),
            Ok(Transcript { words: "Bonjour, \"pouvez\"-vous m'entendre ?".into(), seconds: Some(27) }),
            "without the escapes of the answer and the blanks about them"
        );
        let parts = br#"{"text": "Kaese", "usage": {"type": "duration", "seconds": 27.25}}"#;
        assert_eq!(parse_answer(parts).map(|transcript| transcript.seconds), Ok(Some(28)), "a second begun is counted");
        assert_eq!(
            parse_answer(parts).map(|transcript| transcript.paid_seconds(30)),
            Ok(28),
            "and paid for in place of those recorded"
        );
        let tokens =
            br#"{"text": "x", "usage": {"type": "tokens", "input_tokens": 14, "seconds": 3, "output_tokens": 2}}"#;
        assert_eq!(
            parse_answer(tokens),
            Ok(Transcript { words: "x".into(), seconds: None }),
            "a service that counts tokens tells no seconds"
        );
        assert_eq!(
            parse_answer(tokens).map(|transcript| transcript.paid_seconds(30)),
            Ok(30),
            "and the seconds recorded are paid for"
        );
        assert_eq!(
            parse_answer(br#"{"text": "", "languages": []}"#),
            Ok(Transcript { words: String::new(), seconds: None }),
            "silence is written down as nothing"
        );
        let refusal = br#"{"error": {"message": "Invalid file format.", "type": "invalid_request_error", "param": null, "code": null}}"#;
        assert_eq!(
            parse_answer(refusal),
            Err(Error::Refused(Refusal { code: String::new(), message: "Invalid file format.".into() }))
        );
        for body in [&b""[..], b"{}", b"<html>502</html>", b"{\"text\": 7}", &[0xFF, 0xFE]] {
            assert_eq!(parse_answer(body), Err(Error::NoWords), "{:?}", core::str::from_utf8(body));
        }
        let refusal = Refusal { code: "invalid_api_key".into(), message: "sk-...abcd".into() };
        assert!(
            matches!(Error::Refused(refusal).to_fault(401), Fault::Refused(reason) if reason == "status 401, the service does not know the key (invalid_api_key)"),
            "refused, and the service's words on a key never reach the log"
        );
        assert!(
            matches!(Error::NoWords.to_fault(200), Fault::Temporary(reason) if reason == "status 200, no words in the answer")
        );
    }

    #[test]
    fn streamed_words() {
        let body = request_form("XYZ", b"m4a", " Anna, Pixel ", true);
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("name=\"stream\"\r\n\r\ntrue\r\n")
                && text.contains("name=\"prompt\"\r\n\r\nAnna, Pixel\r\n")
                && text.ends_with("filename=\"note.m4a\"\r\nContent-Type: audio/mp4\r\n\r\nm4a\r\n--XYZ--\r\n"),
            "{text}"
        );
        assert!(!String::from_utf8_lossy(&request_form("XYZ", b"x", "  ", true)).contains("prompt"), "nothing to tell");
        let mut stream = Stream::default();
        assert_eq!(
            stream.take(r#"{"type":"transcript.text.delta","delta":"Wie \"wird\""}"#).as_deref(),
            Some("Wie \"wird\"")
        );
        assert_eq!(
            (stream.take("{}"), stream.take("not json"), stream.has_end()),
            (None, None, false),
            "what tells nothing is passed over"
        );
        let done = r#"{"type":"transcript.text.done","text":"Wie wird das Wetter?","usage":{"type":"duration","seconds":3.2}}"#;
        assert_eq!(stream.take(done), None);
        assert!(stream.has_end());
        assert_eq!(
            stream.finish(200, b"").ok(),
            Some(Transcript { words: "Wie wird das Wetter?".into(), seconds: Some(4) }),
            "the words as their end said"
        );
        let mut refusal = Stream::default();
        refusal.take(r#"{"type":"error","error":{"message":"Audio too short.","code":"audio_too_short"}}"#);
        assert!(
            matches!(refusal.finish(200, b""), Err(Fault::Refused(reason)) if reason == "status 200, audio_too_short: Audio too short."),
            "a refusal among the events is their end"
        );
        let body = br#"{"error":{"message":"Bad file.","code":"invalid_value"}}"#;
        assert!(
            matches!(Stream::default().finish(400, body), Err(Fault::Refused(reason)) if reason == "status 400, invalid_value: Bad file."),
            "without their end the body says why there are none"
        );
    }

    #[test]
    fn subjects() {
        let transcript = Transcript { words: "Wie wird".into(), seconds: Some(4) };
        let names = [Subject::Conversation, Subject::Note, Subject::Message, Subject::PokedexName]
            .map(|subject| transcript.to_log_line(900, subject, 120, 4));
        assert_eq!(
            names,
            [
                "openai: 900 bytes of the conversation's sound written down as 8 characters in 120 ms, 4 s paid for",
                "openai: 900 bytes of sound written down as 8 characters in 120 ms, 4 s paid for",
                "openai: 900 bytes of a message said written down as 8 characters in 120 ms, 4 s paid for",
                "openai: 900 bytes of a name said to the Pokédex written down as 8 characters in 120 ms, 4 s paid for",
            ]
        );
        let booked = |subject: Subject, kind: Kind| {
            let mut ledger = Ledger::default();
            subject.book(&mut ledger, (2026, 10), 4, 300);
            (ledger.months[0].paid[kind as usize], ledger.months[0].seconds)
        };
        assert_eq!(
            booked(Subject::Conversation, Kind::Chat),
            (Paid { count: 0, micros: 300 }, 0),
            "a message's words add to the conversation's cost and count no turn"
        );
        assert_eq!(
            booked(Subject::Note, Kind::Note),
            (Paid { count: 0, micros: 300 }, 4),
            "a note's sound adds its seconds and its cost, and counts no note"
        );
        assert_eq!(booked(Subject::Message, Kind::Dictation), (Paid { count: 1, micros: 300 }, 0));
        assert_eq!(booked(Subject::PokedexName, Kind::Pokedex), (Paid { count: 1, micros: 300 }, 0));
        assert_eq!(
            [
                booked(Subject::WebRequest, Kind::Web),
                booked(Subject::BellRequest, Kind::Bell),
                booked(Subject::RundownsRequest, Kind::Rundown)
            ],
            [(Paid { count: 0, micros: 300 }, 0); 3],
            "a request said adds its writing down to the step that reads it, and counts none"
        );
    }

    #[test]
    fn sound_cost() {
        assert_eq!((sound_micros(60), sound_micros(1), sound_micros(0), sound_micros(3600)), (4500, 75, 0, 270_000));
        assert_eq!(
            sound_micros(u64::MAX),
            u64::MAX / SECONDS_PER_MINUTE,
            "a count no answer could hold, the most there is"
        );
    }
}
