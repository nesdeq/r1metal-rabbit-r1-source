use super::Malformed;
use super::head::{Head, INFORMATIONAL};
use super::request::HEAD;
use alloc::vec::Vec;

const CONTENT_LENGTH: &str = "content-length";
const TRANSFER_ENCODING: &str = "transfer-encoding";
const CHUNKED: &str = "chunked";
const NO_CONTENT: u16 = 204;
const NOT_MODIFIED: u16 = 304;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChunkState {
    Length(u64, bool),
    Extension(u64),
    LengthEnd(u64),
    Data(u64),
    DataEnd(bool),
    Trailer(bool, bool),
    End,
}

/// Decoder of a chunked body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chunks(ChunkState);

impl Default for Chunks {
    fn default() -> Self {
        Chunks(ChunkState::Length(0, false))
    }
}

impl Chunks {
    pub(crate) fn take(&mut self, input: &[u8], body: &mut Vec<u8>) -> Result<(usize, bool), Malformed> {
        let mut at = 0;
        while at < input.len() && self.0 != ChunkState::End {
            let byte = input[at];
            if let ChunkState::Data(remaining) = self.0 {
                let count = usize::try_from(remaining).unwrap_or(usize::MAX).min(input.len() - at);
                body.extend(&input[at..at + count]);
                at += count;
                self.0 = if remaining == count as u64 {
                    ChunkState::DataEnd(false)
                } else {
                    ChunkState::Data(remaining - count as u64)
                };
                continue;
            }
            if let (ChunkState::Length(length, _), Some(digit)) = (self.0, (byte as char).to_digit(16)) {
                let length = length.checked_mul(16).and_then(|length| length.checked_add(digit.into()));
                (self.0, at) = (ChunkState::Length(length.ok_or(Malformed)?, true), at + 1);
                continue;
            }
            self.0 = match (self.0, byte) {
                (ChunkState::Length(length, true), b';' | b' ' | b'\t') => ChunkState::Extension(length),
                (ChunkState::Length(length, true) | ChunkState::Extension(length), b'\r') => {
                    ChunkState::LengthEnd(length)
                }
                (ChunkState::Extension(length), _) => ChunkState::Extension(length),
                (ChunkState::LengthEnd(0), b'\n') => ChunkState::Trailer(true, false),
                (ChunkState::LengthEnd(length), b'\n') => ChunkState::Data(length),
                (ChunkState::DataEnd(false), b'\r') => ChunkState::DataEnd(true),
                (ChunkState::DataEnd(true), b'\n') => ChunkState::Length(0, false),
                (ChunkState::Trailer(true, true), b'\n') => ChunkState::End,
                (ChunkState::Trailer(false, true), b'\n') => ChunkState::Trailer(true, false),
                (ChunkState::Trailer(is_empty, false), b'\r') => ChunkState::Trailer(is_empty, true),
                (ChunkState::Trailer(_, false), _) => ChunkState::Trailer(false, false),
                _ => return Err(Malformed),
            };
            at += 1;
        }
        Ok((at, self.0 == ChunkState::End))
    }
}

/// The state of reading an answer's body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Framing {
    Length(u64),
    Chunked(Chunks),
    UntilClose,
    End,
}

fn to_framing(head: &Head, method: &str) -> Result<Framing, Malformed> {
    if method == HEAD
        || INFORMATIONAL.contains(&head.status)
        || head.status == NO_CONTENT
        || head.status == NOT_MODIFIED
    {
        return Ok(Framing::End);
    }
    if let Some(last) = head.tokens(TRANSFER_ENCODING).last() {
        let is_chunked = last.eq_ignore_ascii_case(CHUNKED);
        return Ok(if is_chunked { Framing::Chunked(Chunks::default()) } else { Framing::UntilClose });
    }
    let mut lengths = head.tokens(CONTENT_LENGTH);
    match lengths.next() {
        None => Ok(Framing::UntilClose),
        Some(first) if first.bytes().all(|digit| digit.is_ascii_digit()) && lengths.all(|other| other == first) => {
            match first.parse() {
                Ok(0) => Ok(Framing::End),
                Ok(length) => Ok(Framing::Length(length)),
                Err(_) => Err(Malformed),
            }
        }
        Some(_) => Err(Malformed),
    }
}

impl Framing {
    /// The framing of the body behind a head, and whether the connection ends with the answer.
    pub fn from_head(head: &Head, method: &str) -> Result<(Framing, bool), Malformed> {
        let framing = to_framing(head, method)?;
        Ok((framing, framing == Framing::UntilClose || !head.keeps_alive()))
    }

    pub const fn is_ended(&self) -> bool {
        matches!(self, Framing::End)
    }

    /// True where the connection carries the next request: the body ended, `excess` bytes follow it.
    pub const fn is_reusable(&self, is_closing: bool, excess: usize) -> bool {
        !is_closing && self.is_ended() && excess == 0
    }

    /// Moves the body's bytes from `input` onto `body`; what follows the body stays in `input`.
    pub fn take(&mut self, input: &mut Vec<u8>, body: &mut Vec<u8>) -> Result<(), Malformed> {
        let is_end = match self {
            Framing::Length(remaining) => {
                let count = usize::try_from(*remaining).unwrap_or(usize::MAX).min(input.len());
                body.extend(input.drain(..count));
                *remaining -= count as u64;
                *remaining == 0
            }
            Framing::Chunked(chunks) => {
                let (count, is_end) = chunks.take(input, body)?;
                input.drain(..count);
                is_end
            }
            Framing::UntilClose => {
                body.append(input);
                false
            }
            Framing::End => true,
        };
        if is_end {
            *self = Framing::End;
        }
        Ok(())
    }

    /// The server closed: true where that ends the body, false where it cuts it short.
    pub fn close(&mut self) -> bool {
        let is_end = *self == Framing::UntilClose;
        if is_end {
            *self = Framing::End;
        }
        is_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    #[test]
    fn single_byte_encoding() {
        let latin =
            Head::parse(b"HTTP/1.1 200 Gel\xE4nde\r\nX-Ort: K\xF6ln\r\nContent-Length: 2\r\n\r\nok").unwrap().unwrap();
        assert_eq!(
            (latin.0.status, latin.0.header("x-ort"), to_framing(&latin.0, "GET"), latin.1),
            (200, Some("K\u{FFFD}ln"), Ok(Framing::Length(2)), 56),
            "a reason or a value in a single byte encoding is taken"
        );
    }

    #[test]
    fn framing_from_head() {
        let head = |text: &str| Head::parse(format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        for (text, method, body, reason) in [
            ("200 OK\r\nContent-Length: 42", "GET", Framing::Length(42), "by its length"),
            (
                "200 OK\r\nContent-Length: 42",
                "HEAD",
                Framing::End,
                "an answer to HEAD tells a length and carries nothing",
            ),
            (
                "200 OK\r\nContent-Length: 42\r\nTransfer-Encoding: gzip, Chunked",
                "GET",
                Framing::Chunked(Chunks::default()),
                "chunks count before a length",
            ),
            (
                "200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: gzip",
                "GET",
                Framing::UntilClose,
                "a coding after the chunks leaves only the close",
            ),
            ("200 OK", "GET", Framing::UntilClose, "nothing said"),
            (
                "200 OK\r\nContent-Length: 42, 42\r\nContent-Length: 42",
                "GET",
                Framing::Length(42),
                "the same length said again is one length",
            ),
            ("204 No Content", "GET", Framing::End, "no content"),
            ("304 Not Modified\r\nContent-Length: 9", "GET", Framing::End, "nothing new"),
            ("101 Switching Protocols", "GET", Framing::End, "another protocol"),
        ] {
            assert_eq!(to_framing(&head(text), method), Ok(body), "{reason}");
        }
        for bad in [
            "Content-Length: 42\r\nContent-Length: 43",
            "Content-Length: -1",
            "Content-Length: 4 2",
            "Content-Length: ",
            "Content-Length: 99999999999999999999999",
        ] {
            assert_eq!(to_framing(&head(&format!("200 OK\r\n{bad}")), "GET"), Err(Malformed), "{bad}");
        }
    }

    #[test]
    fn chunks_cut_anywhere() {
        let input = b"4\r\nWiki\r\n6;name=value\r\npedia \r\nE \r\nin \r\n\r\nchunks.\r\n0\r\nTrailer: x\r\n\r\nnext";
        let expected = b"Wikipedia in \r\n\r\nchunks.";
        let mut body = Vec::new();
        assert_eq!(
            Chunks::default().take(input, &mut body),
            Ok((input.len() - 4, true)),
            "what follows the body is left alone"
        );
        assert_eq!(body, expected);
        for cut in 1..input.len() - 4 {
            let (mut chunks, mut body) = (Chunks::default(), Vec::new());
            assert_eq!(chunks.take(&input[..cut], &mut body), Ok((cut, false)), "cut at {cut}");
            assert_eq!(chunks.take(&input[cut..], &mut body), Ok((input.len() - 4 - cut, true)), "cut at {cut}");
            assert_eq!(body, expected, "cut at {cut}");
        }
        let (mut chunks, mut body) = (Chunks::default(), Vec::new());
        assert_eq!(chunks.take(b"0\r\n\r\n", &mut body), Ok((5, true)));
        assert_eq!(
            (chunks.take(b"more", &mut body), body.len()),
            (Ok((0, true)), 0),
            "a body that has ended takes nothing more"
        );
        for bad in [
            &b"\r\n"[..],
            b"g\r\n",
            b"4\nWiki",
            b"4\r\nWikiX",
            b"4\r\nWiki\rX",
            b"4x\r\n",
            b"11111111111111111\r\n",
            b"0\r\n\rX",
        ] {
            assert_eq!(Chunks::default().take(bad, &mut vec![]), Err(Malformed), "{:?}", core::str::from_utf8(bad));
        }
    }

    #[test]
    fn body_out_of_framing() {
        let head = |text: &str| Head::parse(format!("{text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        let read = |framing: &mut Framing, pieces: &[&[u8]]| {
            let (mut input, mut body) = (Vec::new(), Vec::new());
            for piece in pieces {
                input.extend_from_slice(piece);
                framing.take(&mut input, &mut body).unwrap();
            }
            (body, input, framing.is_ended())
        };
        let (mut length, is_closing) =
            Framing::from_head(&head("HTTP/1.1 200 OK\r\nContent-Length: 11"), "GET").unwrap();
        assert_eq!((length, is_closing), (Framing::Length(11), false));
        assert_eq!(
            read(&mut length, &[b"hello", b" world", b"HTTP"]),
            (b"hello world".to_vec(), b"HTTP".to_vec(), true),
            "by its length; what follows waits for the next answer"
        );
        let (mut chunked, _) =
            Framing::from_head(&head("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked"), "GET").unwrap();
        assert_eq!(
            read(&mut chunked, &[b"5\r\nhel", b"lo\r\n0\r\n\r\nnext"]),
            (b"hello".to_vec(), b"next".to_vec(), true)
        );
        let (mut to_close, is_closing) = Framing::from_head(&head("HTTP/1.0 200 OK"), "GET").unwrap();
        assert_eq!(
            (to_close, is_closing),
            (Framing::UntilClose, true),
            "a body that runs to the close ends its connection"
        );
        assert_eq!(read(&mut to_close, &[b"all", b" of it"]), (b"all of it".to_vec(), Vec::new(), false));
        assert!(to_close.close() && to_close.is_ended(), "the close ends it");
        assert!(!to_close.is_reusable(true, 0), "and its connection with it");
        assert!(length.is_reusable(false, 0) && !length.is_reusable(false, 4), "nothing of it nor behind it left");
        assert!(!chunked.is_reusable(true, 0), "nor one its server ends");
        assert!(!Framing::Length(4).is_reusable(false, 0), "nor before its end");
        let mut cut = Framing::Length(4);
        assert!(!cut.close() && !cut.is_ended(), "a close before its length cuts a body short");
        assert_eq!(
            [
                Framing::from_head(&head("HTTP/1.1 204 No Content"), "GET"),
                Framing::from_head(&head("HTTP/1.1 200 OK\r\nContent-Length: 0"), "GET"),
                Framing::from_head(&head("HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close"), "HEAD")
            ],
            [Ok((Framing::End, false)), Ok((Framing::End, false)), Ok((Framing::End, true))],
            "no body has ended at once, and a server that says it closes keeps nothing"
        );
        assert_eq!(Framing::from_head(&head("HTTP/1.1 200 OK\r\nContent-Length: x"), "GET"), Err(Malformed));
        let mut invalid = Framing::Chunked(Chunks::default());
        assert_eq!(invalid.take(&mut b"g\r\n".to_vec(), &mut Vec::new()), Err(Malformed));
    }
}
