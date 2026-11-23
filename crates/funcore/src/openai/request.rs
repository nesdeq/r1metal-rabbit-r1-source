//! A request to OpenAI's services: its address, headers and packed body, the system prompt's modules, and its progress.

use crate::codec::http::OK;
use crate::codec::http::POST;
use crate::codec::http::Target;
use crate::codec::http::USER_AGENT;
use crate::codec::zstd;
use crate::openai::fault::Fault;
use crate::openai::models::image::SIDE;
use crate::openai::sse::EventStream;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const MODULES: &str = include_str!("../../assets/openai/modules.txt");
pub const JSON: &str = "application/json";
pub const EVENTS: &str = "text/event-stream";
const ZSTD: &str = "zstd";
const COMPRESSED_PATH: &str = "/v1/responses";
const MAX_RATIO: usize = 100;
// Three bytes of a pixel's colour in base64.
const CHARACTERS_PER_PIXEL: usize = 4;
/// The most bytes of an answer, or of one event of a stream, that are taken: the largest answer asked for is a redrawn photo in base64, which a JPEG no smaller than its pixels would fill.
pub const ANSWER_BYTES: usize = SIDE * SIDE * CHARACTERS_PER_PIXEL;
/// The error text of an answer that holds more.
pub const TOO_LONG: &str = "the answer holds more than any that is asked for";

/// Refuses an answer that holds `bytes` so far where no answer asked for holds as many.
pub const fn check_length(bytes: usize) -> Result<(), &'static str> {
    if bytes > ANSWER_BYTES { Err(TOO_LONG) } else { Ok(()) }
}

/// The outcome of a streamed request whose connection failed, `is_end` where that came after the answer's end: the answer stands, as a stream with nothing after it.
pub fn finished_stream(is_end: bool) -> Option<(u16, Vec<u8>)> {
    is_end.then(|| (OK, Vec::new()))
}

/// An answer's body as it comes: the events of one that streams, the bytes of one that does not.
pub struct Body {
    events: Option<EventStream>,
    bytes: Vec<u8>,
}

impl Body {
    /// The body of an answer of that status: only one that is the answer asked for streams.
    pub fn new(status: u16) -> Body {
        Body { events: (status == OK).then(EventStream::default), bytes: Vec::new() }
    }

    /// Takes the answer's next bytes and returns the data of every event they complete, or why no more of it is taken.
    pub fn take(&mut self, bytes: &[u8]) -> Result<Vec<String>, &'static str> {
        let mut data = Vec::new();
        let held = match &mut self.events {
            Some(events) => events.take(bytes, |event| data.push(event.into())),
            None => {
                self.bytes.extend_from_slice(bytes);
                self.bytes.len()
            }
        };
        check_length(held)?;
        Ok(data)
    }

    /// The bytes, empty for an answer that streamed.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) fn with_modules(task: &str) -> String {
    format!("{}\n\n{}", task.trim(), MODULES.trim())
}

pub fn parse_target(address: &str) -> Result<Target, Fault> {
    Target::parse(address).ok_or_else(|| Fault::Temporary(format!("{address} is no URL")))
}

/// The headers of a request: its `bearer`, the answer's `accept`, and the body's `kind` and encoding.
pub fn headers<'a>(
    bearer: &'a str,
    accept: &'a str,
    kind: Option<&'a str>,
    is_compressed: bool,
) -> Vec<(&'static str, &'a str)> {
    let mut headers = vec![("User-Agent", USER_AGENT), ("Authorization", bearer), ("Accept", accept)];
    headers.extend(kind.map(|kind| ("Content-Type", kind)));
    headers.extend(is_compressed.then_some(("Content-Encoding", ZSTD)));
    headers
}

pub fn session_headers(bearer: &str) -> [(&'static str, &str); 2] {
    [("User-Agent", USER_AGENT), ("Authorization", bearer)]
}

/// The body compressed with zstd where it goes to the one address OpenAI takes it at and that makes it smaller, within `MAX_RATIO`.
pub fn compress_body(target: &Target, method: &str, kind: Option<&str>, body: &[u8]) -> Option<Vec<u8>> {
    if method != POST || kind != Some(JSON) || target.path != COMPRESSED_PATH {
        return None;
    }
    let output = zstd::compress(body);
    (output.len() < body.len() && body.len() <= output.len() * MAX_RATIO).then_some(output)
}

/// A request's progress, for the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    // A connection to the service is in hand.
    Reached,
    Sent,
    // The answer's head has come.
    Answered,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_streams() {
        assert_eq!(finished_stream(true), Some((OK, Vec::new())), "what came is paid for, and stands");
        assert_eq!(finished_stream(false), None, "an answer cut short before its end is lost");
    }

    #[test]
    fn body_by_status() {
        let mut stream = Body::new(OK);
        assert_eq!(stream.take(b"data: a\n"), Ok(Vec::new()));
        assert_eq!(stream.take(b"\ndata: b\n\n"), Ok(vec!["a".into(), "b".into()]));
        assert!(stream.into_bytes().is_empty());
        let mut refusal = Body::new(429);
        assert_eq!(refusal.take(b"data: a\n\n"), Ok(Vec::new()));
        assert_eq!(refusal.into_bytes(), b"data: a\n\n");
    }

    #[test]
    fn answer_length_bound() {
        assert_eq!(ANSWER_BYTES, 4 << 20, "a photo of 1024 pixels a side, four characters a pixel");
        assert_eq!((check_length(ANSWER_BYTES), check_length(ANSWER_BYTES + 1)), (Ok(()), Err(TOO_LONG)));
        let longest = vec![b'x'; ANSWER_BYTES];
        let mut whole = Body::new(429);
        assert_eq!(whole.take(&longest), Ok(Vec::new()), "an answer as long as the longest asked for is taken");
        assert_eq!(whole.take(b"x"), Err(TOO_LONG), "and not a byte more");
        let mut stream = Body::new(OK);
        assert_eq!(stream.take(&longest), Ok(Vec::new()));
        assert_eq!(stream.take(b"x"), Err(TOO_LONG), "nor an event of a stream that has no end");
        let mut events = Body::new(OK);
        assert_eq!(events.take(&longest), Ok(Vec::new()));
        assert_eq!(
            events.take(b"\n\ndata: next\n\n"),
            Ok(vec!["next".into()]),
            "a stream is as long as it likes: what counts is each event"
        );
    }

    #[test]
    fn request_headers() {
        let plain = headers("Bearer sk-a", JSON, None, false);
        assert_eq!(plain, [("User-Agent", USER_AGENT), ("Authorization", "Bearer sk-a"), ("Accept", JSON)]);
        let with_body = headers("Bearer sk-a", EVENTS, Some(JSON), true);
        assert_eq!(
            with_body[3..],
            [("Content-Type", JSON), ("Content-Encoding", ZSTD)],
            "what its body is, and how it is packed"
        );
        assert_eq!(session_headers("Bearer sk-a"), [("User-Agent", USER_AGENT), ("Authorization", "Bearer sk-a")]);
    }

    #[test]
    fn reasoning_modules() {
        let prompt = with_modules("  Sum it up.\n");
        assert!(prompt.starts_with("Sum it up.\n\nThe reasoning modules below are your private structuring aid"));
        assert!(
            prompt.ends_with("\n39. Let's make a step by step plan and implement it with good notion and explanation.")
        );
        let modules: alloc::vec::Vec<&str> = MODULES.trim().lines().skip(1).collect();
        assert_eq!(modules.len(), 39);
        for (number, module) in modules.iter().enumerate() {
            assert!(module.starts_with(&format!("{}. ", number + 1)), "{module}");
        }
        assert_eq!(modules[0], "1. How could I devise an experiment to help solve that problem?");
        assert!(MODULES.lines().next().is_some_and(|lead| lead.ends_with("(SELF-DISCOVER, arXiv 2402.03620):")));
    }

    #[test]
    fn body_compression() {
        let target = Target::parse("https://api.openai.com/v1/responses").unwrap();
        let thread: String = (0..200u32)
            .map(|at| {
                format!(
                    "{{\"role\":\"user\",\"content\":\"Wie wird das Wetter am {at}. Tag? {} Grad.\"}},",
                    at * 7 % 31
                )
            })
            .collect();
        let output = compress_body(&target, POST, Some(JSON), thread.as_bytes()).unwrap();
        assert!(output.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) && output.len() < thread.len());
        assert_eq!(compress_body(&target, POST, Some(JSON), b"{}"), None, "what packing makes no smaller");
        assert_eq!(
            compress_body(&target, POST, Some(JSON), &[b' '; 4096]),
            None,
            "nor what would shrink to under a hundredth"
        );
        assert_eq!(
            compress_body(&target, crate::codec::http::GET, Some(JSON), thread.as_bytes()),
            None,
            "a look carries nothing"
        );
        assert_eq!(compress_body(&target, POST, Some(EVENTS), thread.as_bytes()), None, "nor what is no JSON");
        let other = Target { path: "/v1/audio/transcriptions".into(), ..target };
        assert_eq!(
            compress_body(&other, POST, Some(JSON), thread.as_bytes()),
            None,
            "nor what goes to another address"
        );
        assert!(matches!(parse_target("no url"), Err(Fault::Temporary(reason)) if reason == "no url is no URL"));
    }
}
