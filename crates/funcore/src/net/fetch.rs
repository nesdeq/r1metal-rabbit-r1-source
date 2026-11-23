//! A document fetched over HTTPS, a feed or a page of the browser's: what its answer's head announces, and the limits of its read.

use super::race;
use crate::codec::http::{AnswerFault, Head, Target};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

/// The largest document taken: some twelve times the largest of the 47 feeds, about 0.3 MB; a page of the browser's, and the pages it keeps to go back to, are held to the same.
pub const MAX_BYTES: usize = 4 << 20;
/// The redirects followed; none of the 47 feeds sends more than one.
pub const MAX_HOPS: usize = 3;
// How long a document is given at one address: three times the silence that ends a wait on a connection.
const TIMEOUT: Duration = race::TIMEOUT.saturating_mul(3);

/// Why a fetch at one address gave no document.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fault {
    // A redirect to no address, or from TLS to plain text.
    BadRedirect,
    Status(u16),
    // The content encoding of the header, which was not asked for.
    Encoded(String),
    TooLarge,
    TooSlow,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::BadRedirect => f.write_str("sent to what is no address"),
            Fault::Status(status) => write!(f, "status {status}"),
            Fault::Encoded(encoding) => write!(f, "a document packed as {encoding}, which was not asked for"),
            Fault::TooLarge => f.write_str("a document larger than is taken"),
            Fault::TooSlow => f.write_str("a document that takes longer than it is given"),
        }
    }
}

impl core::error::Error for Fault {}

impl From<AnswerFault> for Fault {
    fn from(fault: AnswerFault) -> Fault {
        match fault {
            AnswerFault::Status(status) => Fault::Status(status),
            AnswerFault::Encoding(encoding) => Fault::Encoded(encoding),
        }
    }
}

/// What an answer's head announces.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ResponseKind {
    Redirect(Target),
    Document,
}

/// What a fetch at one address gave.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fetched {
    Redirect(Target),
    Document(Vec<u8>),
}

/// Refuses an answer whose head announces no plain document, one that sends the asker on among them.
pub fn check_plain(head: &Head) -> Result<(), Fault> {
    Ok(head.check_document()?)
}

/// What an answer's head announces for a fetch at `target`, or why it is no document's.
pub fn check_head(head: &Head, target: &Target) -> Result<ResponseKind, Fault> {
    if let Some(location) = head.redirect_location() {
        return target.resolve(location).map(ResponseKind::Redirect).ok_or(Fault::BadRedirect);
    }
    check_plain(head)?;
    Ok(ResponseKind::Document)
}

fn check_size(bytes: usize) -> Result<(), Fault> {
    if bytes > MAX_BYTES { Err(Fault::TooLarge) } else { Ok(()) }
}

/// Refuses a read past the size or time limit.
pub fn check_read(bytes: usize, elapsed: Duration) -> Result<(), Fault> {
    check_size(bytes)?;
    if elapsed >= TIMEOUT { Err(Fault::TooSlow) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn head_checks() {
        let head = |text: &str| Head::parse(alloc::format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        let target = Target::parse("https://example.org/rss").unwrap();
        assert_eq!(
            check_head(&head("301 Moved\r\nLocation: /feed/"), &target),
            Ok(ResponseKind::Redirect(Target::parse("https://example.org/feed/").unwrap()))
        );
        assert_eq!(
            [
                check_head(&head("302 Found\r\nLocation: mailto:nobody@example.org"), &target),
                check_head(&head("301 Moved\r\nLocation: http://example.org/feed/"), &target),
                check_head(&head("404 Not Found"), &target),
                check_head(&head("200 OK\r\nContent-Encoding: gzip"), &target)
            ],
            [
                Err(Fault::BadRedirect),
                Err(Fault::BadRedirect),
                Err(Fault::Status(404)),
                Err(Fault::Encoded("gzip".into()))
            ],
            "nowhere, into the clear, another status, and a packing that was not asked for"
        );
        assert_eq!(
            [check_head(&head("200 OK"), &target), check_head(&head("200 OK\r\nContent-Encoding: Identity"), &target)],
            [Ok(ResponseKind::Document), Ok(ResponseKind::Document)]
        );
        assert_eq!(
            [
                check_plain(&head("200 OK")),
                check_plain(&head("429 Too Many Requests")),
                check_plain(&head("301 Moved\r\nLocation: /x/")),
                check_plain(&head("200 OK\r\nContent-Encoding: br"))
            ],
            [Ok(()), Err(Fault::Status(429)), Err(Fault::Status(301)), Err(Fault::Encoded("br".into()))],
            "where no asker is sent on, a status that says so is another status"
        );
    }

    #[test]
    fn read_limits() {
        assert_eq!((check_size(MAX_BYTES), check_size(MAX_BYTES + 1)), (Ok(()), Err(Fault::TooLarge)));
        assert_eq!(TIMEOUT, Duration::from_secs(30));
        assert_eq!(check_read(MAX_BYTES, TIMEOUT - Duration::from_secs(1)), Ok(()));
        assert_eq!(check_read(MAX_BYTES + 1, Duration::ZERO), Err(Fault::TooLarge));
        assert_eq!(check_read(0, TIMEOUT), Err(Fault::TooSlow));
    }

    #[test]
    fn fault_words() {
        assert_eq!(
            [Fault::BadRedirect, Fault::Status(404), Fault::Encoded("gzip".into()), Fault::TooLarge, Fault::TooSlow]
                .map(|fault| fault.to_string()),
            [
                "sent to what is no address",
                "status 404",
                "a document packed as gzip, which was not asked for",
                "a document larger than is taken",
                "a document that takes longer than it is given"
            ]
        );
    }
}
