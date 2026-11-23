//! Lines on the wire: their limit, the host's lines off the port, and answers made and read back.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The longest line from the host; an append, its longest, gives half to its data and the rest to its path.
pub const LINE_BYTES: usize = 8192;
/// The answer to a line without an end within `LINE_BYTES`.
pub const TOO_LONG: &str = "a line without end";
pub(super) const LOG_PREFIX: &str = "# ";
const DATA_PREFIX: &str = "> ";
const OK: &str = "ok";
const ERR_PREFIX: &str = "err ";
const LINE_END: &str = "\r\n";

pub(super) fn frame<'a>(prefix: &'a str, text: &'a str) -> impl Iterator<Item = u8> + 'a {
    prefix.bytes().chain(text.bytes()).chain(LINE_END.bytes())
}

/// An answer's wire bytes, made away from the port.
#[derive(Default)]
pub struct Reply {
    bytes: Vec<u8>,
}

impl Reply {
    /// Adds a line of data.
    pub fn data(&mut self, text: &str) {
        self.bytes.extend(frame(DATA_PREFIX, text));
    }

    /// Adds each line of a text.
    pub fn data_text(&mut self, text: &[u8]) {
        String::from_utf8_lossy(text).lines().for_each(|line| self.data(line));
    }

    /// Ends the answer of a done command.
    pub fn ok(&mut self) -> &mut Reply {
        self.bytes.extend(frame("", OK));
        self
    }

    /// Ends the answer of a refused command.
    pub fn err(&mut self, reason: &str) -> &mut Reply {
        self.bytes.extend(frame(ERR_PREFIX, reason));
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A line of the host's as it is taken off the port: a command's, or the place of one dropped for its length, which is answered in its turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Line {
    Command(String),
    TooLong,
}

impl From<&str> for Line {
    fn from(line: &str) -> Line {
        Line::Command(line.to_string())
    }
}

/// Takes the complete lines out of `inbox`, in their order; a line over `LINE_BYTES` is dropped with all that still comes of it, and stands among them once, where its drop began.
pub fn take_lines(inbox: &mut Vec<u8>, is_dropping: &mut bool) -> Vec<Line> {
    let mut lines = Vec::new();
    if let Some(last_newline) = inbox.iter().rposition(|&byte| byte == b'\n') {
        let complete =
            inbox[..last_newline].split(|&byte| byte == b'\n').skip(usize::from(core::mem::take(is_dropping)));
        for line in complete {
            lines.push(if line.len() > LINE_BYTES {
                Line::TooLong
            } else {
                Line::Command(String::from_utf8_lossy(line).trim_end_matches('\r').to_string())
            });
        }
        inbox.drain(..=last_newline);
    }
    if inbox.len() > LINE_BYTES {
        inbox.clear();
        if !core::mem::replace(is_dropping, true) {
            lines.push(Line::TooLong);
        }
    }
    lines
}

/// A device line as the host reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplyLine<'a> {
    Log(&'a str),
    Data(&'a str),
    Ok,
    Err(&'a str),
    Unknown(&'a str),
}

pub fn classify(line: &str) -> ReplyLine<'_> {
    let line = line.trim_matches(['\r', '\n']);
    if let Some(text) = line.strip_prefix(LOG_PREFIX) {
        ReplyLine::Log(text)
    } else if let Some(text) = line.strip_prefix(DATA_PREFIX) {
        ReplyLine::Data(text)
    } else if let Some(text) = line.strip_prefix(ERR_PREFIX) {
        ReplyLine::Err(text)
    } else if line == OK {
        ReplyLine::Ok
    } else {
        ReplyLine::Unknown(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn classified_lines() {
        assert_eq!(
            ["# booted", "> 87", "ok\r\n", "err no such file", "noise"].map(classify),
            [
                ReplyLine::Log("booted"),
                ReplyLine::Data("87"),
                ReplyLine::Ok,
                ReplyLine::Err("no such file"),
                ReplyLine::Unknown("noise")
            ]
        );
    }

    #[test]
    fn complete_lines_only() {
        let (mut inbox, mut is_dropping) = (b"ping\r\nls /sys\npart".to_vec(), false);
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), ["ping".into(), "ls /sys".into()]);
        assert_eq!(inbox, b"part");
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), []);
        inbox.extend_from_slice(b"ial\n");
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), ["partial".into()]);
        assert!(inbox.is_empty() && !is_dropping);
    }

    #[test]
    fn unended_line_dropped_whole() {
        let (mut inbox, mut is_dropping) = (b"ping\n".to_vec(), false);
        inbox.extend(core::iter::repeat_n(b'x', LINE_BYTES));
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), ["ping".into()]);
        assert!(!is_dropping && inbox.len() == LINE_BYTES, "the host tool's longest line still fits");
        inbox.push(b'x');
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), [Line::TooLong], "the host is told once");
        assert!(is_dropping && inbox.is_empty(), "past that, what waits is let go");
        inbox.extend(core::iter::repeat_n(b'y', LINE_BYTES + 1));
        assert_eq!(take_lines(&mut inbox, &mut is_dropping), []);
        assert!(is_dropping && inbox.is_empty(), "and so is what still comes of that line");
        inbox.extend_from_slice(b"its tail\nping\npa");
        let taken = take_lines(&mut inbox, &mut is_dropping);
        assert_eq!(taken, ["ping".into()], "the line after it is a command again");
        assert!(!is_dropping && inbox == b"pa");
    }

    #[test]
    fn long_line_with_end_dropped() {
        let mut inbox = alloc::vec![b'z'; LINE_BYTES + 1];
        inbox.extend_from_slice(b"\nping\n");
        let mut is_dropping = false;
        let mut both = b"ls /sys\n".to_vec();
        both.append(&mut inbox);
        assert_eq!(
            take_lines(&mut both, &mut is_dropping),
            ["ls /sys".into(), Line::TooLong, "ping".into()],
            "the dropped line in its place among the lines, so that its answer comes in its turn"
        );
        assert!(!is_dropping && both.is_empty(), "the line after it is a command");
        let mut longest = alloc::vec![b'z'; LINE_BYTES];
        longest.push(b'\n');
        let taken = take_lines(&mut longest, &mut is_dropping);
        assert!(matches!(&taken[..], [Line::Command(_)]), "a line as long as the most is taken");
    }

    #[test]
    fn blank_lines_kept() {
        let mut inbox = b"\n\n".to_vec();
        assert_eq!(take_lines(&mut inbox, &mut false), ["".into(), "".into()]);
    }

    #[test]
    fn text_lines() {
        let mut reply = Reply::default();
        reply.data_text(b"one\ntwo\r\n");
        assert_eq!(reply.bytes(), format!("{DATA_PREFIX}one\r\n{DATA_PREFIX}two\r\n").as_bytes());
    }

    #[test]
    fn answer_bytes() {
        let mut reply = Reply::default();
        reply.data("one");
        reply.data("");
        reply.ok();
        assert_eq!(reply.bytes(), format!("{DATA_PREFIX}one\r\n{DATA_PREFIX}\r\n{OK}\r\n").as_bytes());
        let refusal = Reply::default().err("no such file").bytes().to_vec();
        assert_eq!(refusal, format!("{ERR_PREFIX}no such file\r\n").as_bytes());
    }
}
