use super::Malformed;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

const HEAD_END: &[u8] = b"\r\n\r\n";
const HEAD_BYTES: usize = 65536;
const STATUS_LINE_START: &str = "HTTP/1.";
const HTTP_1_0_START: &str = "HTTP/1.0 ";
const CONNECTION: &str = "connection";
pub const CLOSE: &str = "close";
const KEEP_ALIVE: &str = "keep-alive";
const WHITESPACE: [char; 2] = [' ', '\t'];
pub(super) const INFORMATIONAL: Range<u16> = 100..200;
pub(crate) const SWITCHING_PROTOCOLS: u16 = 101;
pub(crate) const OK: u16 = 200;
// The classes of RFC 9110, 15: an answer that succeeded, one that faults the request, one that faults the server.
pub(crate) const SUCCESSFUL: Range<u16> = 200..300;
pub(crate) const CLIENT_ERRORS: Range<u16> = 400..500;
pub(crate) const SERVER_ERRORS: Range<u16> = 500..600;
const REDIRECTS: [u16; 5] = [301, 302, 303, 307, 308];
pub(crate) const UNAUTHORIZED: u16 = 401;
pub(crate) const FORBIDDEN: u16 = 403;
pub(crate) const NOT_FOUND: u16 = 404;
pub(crate) const REQUEST_TIMEOUT: u16 = 408;
pub(crate) const CONFLICT: u16 = 409;
pub(crate) const GONE: u16 = 410;
pub(crate) const LENGTH_REQUIRED: u16 = 411;
pub(crate) const PRECONDITION_REQUIRED: u16 = 428;
pub(crate) const TOO_MANY_REQUESTS: u16 = 429;
pub(crate) const SERVER_ERROR: u16 = 500;
const LOCATION: &str = "location";
pub(crate) const IDENTITY: &str = "identity";
const CONTENT_ENCODING: &str = "content-encoding";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Head {
    pub status: u16,
    headers: Vec<(String, String)>,
    is_http_1_0: bool,
}

/// Why no head can be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeadError {
    NoHttp,
    TooLong,
}

impl HeadError {
    /// The log text.
    pub const fn message(self) -> &'static str {
        match self {
            HeadError::NoHttp => "the server speaks no HTTP",
            HeadError::TooLong => "the head of the answer has no end",
        }
    }
}

/// Why an answer is not the document asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AnswerFault {
    Status(u16),
    Encoding(String),
}

impl Head {
    /// Takes the first final head off `input`; none until its end has come.
    pub fn take(input: &mut Vec<u8>) -> Result<Option<Head>, HeadError> {
        loop {
            match Head::parse(input).map_err(|_| HeadError::NoHttp)? {
                Some((head, length)) => {
                    input.drain(..length);
                    if !head.is_interim() {
                        return Ok(Some(head));
                    }
                }
                None if input.len() > HEAD_BYTES => return Err(HeadError::TooLong),
                None => return Ok(None),
            }
        }
    }

    pub(crate) fn parse(input: &[u8]) -> Result<Option<(Head, usize)>, Malformed> {
        let Some(end) = input.windows(HEAD_END.len()).position(|window| window == HEAD_END) else {
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&input[..end]);
        let mut lines = text.split("\r\n");
        let first = lines.next().unwrap_or_default();
        let status = Some(first)
            .and_then(|line| line.strip_prefix(STATUS_LINE_START))
            .filter(|line| line.as_bytes().first().is_some_and(u8::is_ascii_digit))
            .and_then(|line| line.split(' ').nth(1))
            .filter(|status| status.len() == 3 && status.bytes().all(|digit| digit.is_ascii_digit()));
        let status = status.and_then(|status| status.parse().ok()).ok_or(Malformed)?;
        let headers = lines.map(|line| {
            let (name, value) = line
                .split_once(':')
                .filter(|(name, _)| !name.is_empty() && !name.contains(WHITESPACE))
                .ok_or(Malformed)?;
            Ok((name.to_ascii_lowercase(), value.trim_matches(WHITESPACE).to_string()))
        });
        let head = Head {
            status,
            headers: headers.collect::<Result<_, _>>()?,
            is_http_1_0: first.starts_with(HTTP_1_0_START),
        };
        Ok(Some((head, end + HEAD_END.len())))
    }

    pub(super) fn keeps_alive(&self) -> bool {
        !self.has_token(CONNECTION, CLOSE) && (!self.is_http_1_0 || self.has_token(CONNECTION, KEEP_ALIVE))
    }

    pub(crate) fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers.iter().filter(move |(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub(crate) fn header<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        self.header_values(name).next()
    }

    fn content_encoding(&self) -> Option<&str> {
        self.header(CONTENT_ENCODING).filter(|encoding| !encoding.eq_ignore_ascii_case(IDENTITY))
    }

    pub(crate) fn check_document(&self) -> Result<(), AnswerFault> {
        if self.status != OK {
            return Err(AnswerFault::Status(self.status));
        }
        self.content_encoding().map_or(Ok(()), |encoding| Err(AnswerFault::Encoding(encoding.into())))
    }

    pub(super) fn tokens<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.header_values(name).flat_map(|value| value.split(',')).map(|item| item.trim_matches(WHITESPACE))
    }

    pub(crate) fn has_token(&self, name: &str, word: &str) -> bool {
        self.tokens(name).any(|token| token.eq_ignore_ascii_case(word))
    }

    pub(crate) fn redirect_location(&self) -> Option<&str> {
        REDIRECTS.contains(&self.status).then(|| self.header(LOCATION)).flatten()
    }

    fn is_interim(&self) -> bool {
        INFORMATIONAL.contains(&self.status) && self.status != SWITCHING_PROTOCOLS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    #[test]
    fn redirect_location() {
        let head = |text: &str| Head::parse(format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        assert_eq!(
            head("301 Moved Permanently\r\nLocation: https://example.org/feed/").redirect_location(),
            Some("https://example.org/feed/")
        );
        assert_eq!(
            [302, 303, 307, 308]
                .map(|status| head(&format!("{status} X\r\nlocation: /a")).redirect_location() == Some("/a")),
            [true; 4],
            "by any of the statuses that say so, however the header is written"
        );
        assert_eq!(
            (
                head("200 OK\r\nLocation: /a").redirect_location(),
                head("301 Moved").redirect_location(),
                head("304 Not Modified\r\nLocation: /a").redirect_location()
            ),
            (None, None, None),
            "an answer that is the thing asked for sends nowhere, nor does one that names no address"
        );
    }

    #[test]
    fn parse_head() {
        let input = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: a=1\r\nset-cookie:b=2 \r\nConnection: keep-alive, Upgrade\r\n\r\nbody";
        for length in 0..input.len() - 4 {
            assert_eq!(Head::parse(&input[..length]), Ok(None), "{length} bytes");
        }
        let (head, length) = Head::parse(input).unwrap().unwrap();
        assert_eq!((head.status, &input[length..]), (200, &b"body"[..]));
        assert_eq!(
            (head.header("content-type"), head.header("Content-Type"), head.header("etag")),
            (Some("text/plain"), Some("text/plain"), None)
        );
        assert_eq!(head.header_values("Set-Cookie").collect::<Vec<_>>(), ["a=1", "b=2"]);
        assert!(
            head.has_token("connection", "upgrade")
                && head.has_token("Connection", "Keep-Alive")
                && !head.has_token("connection", "close")
        );
        assert_eq!(
            Head::parse(b"HTTP/1.0 404 Not Found\r\n\r\n")
                .map(|parsed| parsed.map(|(head, length)| (head.status, length))),
            Ok(Some((404, 26)))
        );
        for bad in [
            &b"HTTP/2 200 OK\r\n\r\n"[..],
            b"HTTP/1.1 20 OK\r\n\r\n",
            b"HTTP/1.1 2000 OK\r\n\r\n",
            b"HTTP/1.1 abc OK\r\n\r\n",
            b"HTTP/1.1 +20 OK\r\n\r\n",
            b"ICY 200 OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n folded: x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nname : x\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n: x\r\n\r\n",
            b"HTTP/1.\xFF 200 OK\r\n\r\n",
        ] {
            assert_eq!(Head::parse(bad), Err(Malformed), "{:?}", core::str::from_utf8(bad));
        }
    }

    #[test]
    fn keep_alive() {
        let head = |text: &str| Head::parse(format!("{text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        assert!(head("HTTP/1.1 200 OK").keeps_alive(), "1.1 keeps it by itself");
        assert!(!head("HTTP/1.1 200 OK\r\nConnection: close").keeps_alive());
        assert!(!head("HTTP/1.1 200 OK\r\nConnection: Upgrade, Close").keeps_alive(), "a close among other words");
        assert!(!head("HTTP/1.0 200 OK").keeps_alive(), "1.0 closes unless it says otherwise");
        assert!(head("HTTP/1.0 200 OK\r\nConnection: keep-alive").keeps_alive());
    }

    #[test]
    fn take_skips_interim() {
        let head = |text: &str| Head::parse(format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        assert!(
            head("103 Early Hints").is_interim()
                && !head("200 OK").is_interim()
                && !head("101 Switching Protocols").is_interim()
        );
        let bytes =
            b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nno";
        let mut input = bytes[..30].to_vec();
        assert_eq!(Head::take(&mut input), Ok(None), "half a head is none yet");
        input.extend_from_slice(&bytes[30..60]);
        assert_eq!(Head::take(&mut input), Ok(None), "nor is the interim one alone");
        assert!(input.starts_with(b"HTTP/1.1 404"), "which is taken off all the same");
        input.extend_from_slice(&bytes[60..]);
        assert_eq!(Head::take(&mut input).map(|head| head.map(|head| head.status)), Ok(Some(404)));
        assert_eq!(input, b"no");
        let mut switching = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n\x81".to_vec();
        assert_eq!(
            (Head::take(&mut switching).map(|head| head.map(|head| head.status)), switching),
            (Ok(Some(101)), vec![0x81]),
            "a switch is the last answer: what follows is another protocol's"
        );
        assert_eq!(Head::take(&mut b"SSH-2.0-OpenSSH\r\n\r\n".to_vec()), Err(HeadError::NoHttp));
        let mut long = b"HTTP/1.1 200 OK\r\n".to_vec();
        long.resize(HEAD_BYTES, b'x');
        assert_eq!(Head::take(&mut long.clone()), Ok(None), "a head may be as long as the longest");
        long.push(b'x');
        assert_eq!(Head::take(&mut long), Err(HeadError::TooLong), "and no longer");
    }
}
