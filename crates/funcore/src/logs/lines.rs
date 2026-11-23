//! Lines of the log files: stamps, notes, and the end of a file a dead kernel left.

use super::CRASH;
use crate::base::time::MILLIS_PER_SECOND;
use crate::base::time::Moment;
use crate::base::time::offset::to_local;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

pub(super) const NOTE: &str = "--";
const NAMELESS_BOOT: &str = "of no name";
pub const LINE_END: &[u8] = b"\n";

/// A line of the log that is no record of the kernel's.
pub fn note(text: &str) -> String {
    format!("{NOTE} {text}")
}

/// The note a new boot begins with.
pub fn boot_note(boot: Option<&str>) -> String {
    note(&format!("boot {}", boot.unwrap_or(NAMELESS_BOOT)))
}

/// Adds a line stamped with the local time of day it was made at.
pub fn add_line(lines: &mut String, (local, millis): (u64, u32), text: &str) {
    write_stamp(lines, local, millis);
    lines.push_str(text);
    lines.push('\n');
}

pub(super) fn write_stamp(line: &mut String, local: u64, millis: u32) {
    let moment = Moment::from_seconds(local);
    let millis = millis % MILLIS_PER_SECOND as u32;
    let _ = write!(line, "{:02}:{:02}:{:02}.{millis:03} ", moment.hour, moment.minute, moment.second);
}

/// A time in milliseconds since 1970 as a stamp takes it: its local seconds, clocks `offset` minutes ahead, and its milliseconds.
pub const fn to_stamp_time(millis: u64, offset: i16) -> (u64, u32) {
    (to_local(millis / MILLIS_PER_SECOND, offset), (millis % MILLIS_PER_SECOND) as u32)
}

/// A text after the stamp of the local time of day it was made at.
pub fn to_stamped((local, millis): (u64, u32), text: &str) -> String {
    let mut line = String::new();
    write_stamp(&mut line, local, millis);
    line + text
}

fn mend_point(piece: &[u8]) -> Option<(usize, bool)> {
    piece.iter().rposition(|byte| *byte != 0).map(|at| (at + 1, piece[at] != b'\n'))
}

/// A walk back from a file's end to where the zero bytes a dead kernel left begin.
pub struct Mend {
    size: u64,
    end: u64,
    is_open: Option<bool>,
}

impl Mend {
    pub const fn new(size: u64) -> Mend {
        Mend { size, end: size, is_open: None }
    }

    /// The next piece to read, its offset and length, at most `block` bytes; none once the walk is over.
    pub fn next_piece(&self, block: usize) -> Option<(u64, usize)> {
        if self.end == 0 || self.is_open.is_some() {
            return None;
        }
        let start = self.end.saturating_sub(block as u64);
        Some((start, (self.end - start) as usize))
    }

    /// Takes the piece read at `start`.
    pub fn take(&mut self, start: u64, piece: &[u8]) {
        let point = mend_point(piece);
        self.is_open = point.map(|(_, is_open)| is_open);
        self.end = start + point.map_or(0, |(length, _)| length as u64);
    }

    /// The offset to cut the file at and whether its last line then needs an end; none if nothing is cut.
    pub fn cut(&self) -> Option<(u64, bool)> {
        (self.end < self.size).then_some((self.end, self.is_open == Some(true)))
    }

    pub const fn lost_bytes(&self) -> u64 {
        self.size - self.end
    }
}

/// The log line of bytes cut from a day's files; none for no bytes.
pub fn to_cut_line(cut: u64, day: &str) -> Option<String> {
    (cut > 0).then(|| {
        format!(
            "logs: {cut} bytes of {day} were written before a kernel died and never committed, they are gone; what the kernel said last is in {CRASH}"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_format() {
        let mut line = String::from("x");
        write_stamp(&mut line, 1_791_697_759, 31);
        assert_eq!(line, "x05:49:19.031 ");
        let mut line = String::new();
        write_stamp(&mut line, 86_399, 999);
        write_stamp(&mut line, 0, 1000);
        assert_eq!(line, "23:59:59.999 00:00:00.000 ");
    }

    #[test]
    fn lines_and_notes() {
        let mut lines = String::new();
        add_line(&mut lines, (0, 5), "the logs are on");
        add_line(&mut lines, (61, 0), &note("sleep"));
        assert_eq!(lines, "00:00:00.005 the logs are on\n00:01:01.000 -- sleep\n");
        assert_eq!((boot_note(Some("1f2e")), boot_note(None)), ("-- boot 1f2e".into(), "-- boot of no name".into()));
    }

    #[test]
    fn stamp_time() {
        assert_eq!(to_stamp_time(3_600_250, 60), (7_200, 250));
        assert_eq!(to_stamped((61, 5), "text"), "00:01:01.005 text");
    }

    #[test]
    fn mend_walk() {
        let mut mend = Mend::new(10);
        assert_eq!(mend.next_piece(4), Some((6, 4)));
        mend.take(6, &[0; 4]);
        assert_eq!(mend.next_piece(4), Some((2, 4)));
        mend.take(2, b"a\n\0\0");
        assert_eq!((mend.next_piece(4), mend.cut(), mend.lost_bytes()), (None, Some((4, false)), 6));
        let mut cut = Mend::new(4);
        cut.take(0, b"cut\0");
        assert_eq!((cut.cut(), cut.lost_bytes()), (Some((3, true)), 1));
        let mut clean = Mend::new(2);
        clean.take(0, b"a\n");
        assert_eq!((clean.cut(), clean.lost_bytes()), (None, 0));
        assert_eq!(Mend::new(0).next_piece(4), None);
    }

    #[test]
    fn mend_points() {
        assert_eq!(mend_point(b"a line\n\0\0\0"), Some((7, false)), "a line ended where the zeros begin");
        assert_eq!(mend_point(b"a line cut\0"), Some((10, true)), "a line that was cut off is to be ended");
        assert_eq!(mend_point(b"a line\n"), Some((7, false)), "a file that ends in a line is left as it is");
        assert_eq!((mend_point(&[0; 5]), mend_point(b"")), (None, None), "zeros alone have the piece before looked at");
    }
}
