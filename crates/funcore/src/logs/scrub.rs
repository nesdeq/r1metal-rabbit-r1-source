//! The traces of Signal in a day's files, and their removal once the phone unlinks the R1.

use super::records::parse_record;
use super::{CRASH, LOG, is_archive_name};
use crate::base::time::stamp::is_compact_day;
use crate::storage::paths::is_half_written;
use alloc::vec::Vec;

const SIGNAL_LINE: &[u8] = b"r1metal: signal: ";
const BLANK: u8 = b' ';

/// What an entry of the logs' directory is to a scrub, by its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    HalfWritten,
    // A day's name: a folder of that name is scrubbed.
    Day,
    Archive,
    Other,
}

pub fn to_entry(name: &str) -> Entry {
    if is_half_written(name) {
        Entry::HalfWritten
    } else if is_compact_day(name) {
        Entry::Day
    } else if is_archive_name(name) {
        Entry::Archive
    } else {
        Entry::Other
    }
}

/// What a day's file is to a scrub, by its path with its day first (`20261011/log`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Holding {
    HalfWritten,
    // The day's log, scrubbed line by line.
    Log,
    // A crash's copy, blanked where it stands.
    CrashCopy,
    // A file that holds nothing of Signal's.
    Nothing,
}

pub fn to_holding(path: &str) -> Holding {
    if is_half_written(path) {
        return Holding::HalfWritten;
    }
    let mut names = path.split('/').skip(1);
    match (names.next(), names.next(), names.next()) {
        (Some(LOG), None, _) => Holding::Log,
        (Some(CRASH), Some(_), None) => Holding::CrashCopy,
        _ => Holding::Nothing,
    }
}

/// True where the mark of a scrub has done its work: a crash's copies were taken, blanked, in another boot than the scrub's, so that the crash store holds no line from before the scrub any more.
pub fn is_mark_spent(scrubbed: &str, boot: Option<&str>, are_copies_taken: bool) -> bool {
    are_copies_taken && boot.is_some_and(|boot| boot != scrubbed)
}

fn contains(line: &[u8], part: &[u8]) -> bool {
    line.windows(part.len()).any(|at| at == part)
}

fn has_thread_tag(line: &[u8], name: &str) -> bool {
    line.iter().enumerate().filter(|(_, byte)| **byte == b'[').any(|(at, _)| {
        let rest = &line[at + 1..];
        let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
        let tail = rest[digits..].strip_prefix(b":").and_then(|tail| tail.strip_prefix(name.as_bytes()));
        digits > 0 && tail.is_some_and(|tail| tail.first() == Some(&b']'))
    })
}

fn is_signal_line(line: &[u8], threads: &[&str]) -> bool {
    contains(line, SIGNAL_LINE) || threads.iter().any(|name| has_thread_tag(line, name))
}

fn record_head(line: &[u8]) -> Option<&[u8]> {
    let text = core::str::from_utf8(line).ok()?;
    let (stamp, record) = text.split_once(' ')?;
    parse_record(record)?;
    Some(&line[..stamp.len() + 1 + record.find(';')? + 1])
}

/// A day's log taken as it is read, in pieces of any length, without the traces of Signal: it holds no more of the log than a line.
#[derive(Default)]
pub struct LogScrub {
    // The beginning of a line whose end is still to come.
    line: Vec<u8>,
    is_dropping: bool,
    is_changed: bool,
}

impl LogScrub {
    /// Takes the log's next bytes, and adds to `kept` what stays of the lines that end in them.
    pub fn add(&mut self, bytes: &[u8], threads: &[&str], kept: &mut Vec<u8>) {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if !part.ends_with(b"\n") {
                self.line.extend_from_slice(part);
            } else if self.line.is_empty() {
                self.take_line(part, threads, kept);
            } else {
                let mut line = core::mem::take(&mut self.line);
                line.extend_from_slice(part);
                self.take_line(&line, threads, kept);
            }
        }
    }

    /// Adds to `kept` what stays of a last line that has no end.
    pub fn finish(&mut self, threads: &[&str], kept: &mut Vec<u8>) {
        let line = core::mem::take(&mut self.line);
        if !line.is_empty() {
            self.take_line(&line, threads, kept);
        }
    }

    fn take_line(&mut self, line: &[u8], threads: &[&str], kept: &mut Vec<u8>) {
        if self.is_dropping && line.first() == Some(&b' ') {
            return;
        }
        self.is_dropping = is_signal_line(line, threads);
        if !self.is_dropping {
            return kept.extend_from_slice(line);
        }
        self.is_changed = true;
        if let Some(head) = record_head(line) {
            kept.extend_from_slice(head);
            kept.push(b'\n');
        }
    }

    /// True once a line lost anything.
    pub const fn is_changed(&self) -> bool {
        self.is_changed
    }
}

/// Blanks the lines of a crash's copy that hold traces of Signal, their line ends kept, so that the bootloader's layout holds; true if any did.
pub fn blank_signal(copy: &mut [u8], threads: &[&str]) -> bool {
    let mut has_blanks = false;
    for line in copy.split_mut(|byte| *byte == b'\n').filter(|line| is_signal_line(line, threads)) {
        line.fill(BLANK);
        has_blanks = true;
    }
    has_blanks
}

#[cfg(test)]
mod tests {
    use super::super::records::last_number;
    use super::super::{FRAMES, POWER, STATE};
    use super::*;
    use alloc::format;
    use alloc::string::String;

    const THREADS: [&str; 4] = ["signal", "auto", "drafts", "drafter"];

    #[test]
    fn entry_kinds() {
        assert_eq!(
            ["log.new", "20261011", "20261011.tar.gz", "boot"].map(to_entry),
            [Entry::HalfWritten, Entry::Day, Entry::Archive, Entry::Other]
        );
    }

    fn scrub_log(log: &[u8]) -> Option<Vec<u8>> {
        let scrubbed = [log.len().max(1), 7, 1].map(|size| {
            let (mut scrub, mut kept) = (LogScrub::default(), Vec::new());
            log.chunks(size).for_each(|piece| scrub.add(piece, &THREADS, &mut kept));
            scrub.finish(&THREADS, &mut kept);
            scrub.is_changed().then_some(kept)
        });
        assert!(scrubbed[0] == scrubbed[1] && scrubbed[0] == scrubbed[2], "the same however the log is cut in pieces");
        scrubbed.into_iter().next().flatten()
    }

    #[test]
    fn holdings_by_path() {
        let day = |path: &str| format!("20261011/{path}");
        assert_eq!(to_holding(&day(LOG)), Holding::Log, "the day's log, by its path behind the day");
        assert_eq!(to_holding(&day(&format!("{CRASH}/081200-dmesg-ramoops-0"))), Holding::CrashCopy);
        for path in [POWER, FRAMES, STATE, "crash/deeper/x"].map(day).iter().chain([&LOG.into()]) {
            assert_eq!(to_holding(path), Holding::Nothing, "{path:?} holds nothing of Signal's");
        }
        assert_eq!(
            [to_holding(&day("log.new")), to_holding(&day("crash/081200-expdb.new"))],
            [Holding::HalfWritten; 2],
            "what a cut write left, whatever it was to be"
        );
    }

    #[test]
    fn mark_spent_in_a_later_boot() {
        assert!(is_mark_spent("1f2e", Some("9a0b"), true), "the copies of a later boot are taken");
        assert!(!is_mark_spent("1f2e", Some("1f2e"), true), "the scrub's own boot may yet crash with its lines");
        assert!(!is_mark_spent("1f2e", Some("9a0b"), false), "copies that failed are taken at a later start");
        assert!(!is_mark_spent("1f2e", None, true), "a boot without a name may be the scrub's");
    }

    #[test]
    fn signal_lines() {
        for trace in [
            "14,90,7,-; (2)[412:signal]r1metal: signal: the stay ended: gone",
            "14,91,7,-; (1)[530:drafts]r1metal: web: api.openai.com named in 3 ms",
            "14,92,7,-; (0)[531:auto]r1metal: openai: a turn",
            "14,93,7,-; (3)[600:drafter]r1metal: openai: written down",
            "14,94,7,-; (1)[356:init]r1metal: signal: a frame that is none of the service's",
            "<6>[  9.1]  (2)[412:signal]r1metal: names: chat.signal.org",
        ] {
            assert!(is_signal_line(trace.as_bytes(), &THREADS), "{trace:?}");
        }
        for other in [
            "14,95,7,-; (6)[356:battery]r1metal: battery: 73% Charging",
            "6,96,7,-; wlan: signal strength -61 dBm",
            "14,97,7,-; (6)[356:autosave]r1metal: x",
            "14,98,7,-; (6)[signal]r1metal: x",
            "1234 (signal) S 1 1 0",
        ] {
            assert!(!is_signal_line(other.as_bytes(), &THREADS), "{other:?}");
        }
    }

    #[test]
    fn log_keeps_record_heads() {
        let log = "10:00:00.000 6,1,5,-;usb: up\n SUBSYSTEM=platform\n10:00:01.000 14,2,9,-; (2)[412:signal]r1metal: signal: linked as device 3\n SUBSYSTEM=none\n10:00:02.000 -- r1metal: signal: no record\n10:00:03.000 14,3,12,-; (6)[356:battery]r1metal: battery: 73%\n10:00:04.000 14,4,15,-; (2)[412:signal]r1metal: web: chat.signal.org named in 3 ms\n";
        let actual = String::from_utf8(scrub_log(log.as_bytes()).unwrap()).unwrap();
        assert_eq!(
            actual,
            "10:00:00.000 6,1,5,-;usb: up\n SUBSYSTEM=platform\n10:00:01.000 14,2,9,-;\n10:00:03.000 14,3,12,-; (6)[356:battery]r1metal: battery: 73%\n10:00:04.000 14,4,15,-;\n"
        );
        assert_eq!(last_number(actual.as_bytes(), 0), Some(4), "the records are numbered as before");
        assert_eq!(scrub_log(actual.as_bytes()), None, "nothing more to take out");
        assert_eq!(scrub_log(b"10:00:00.000 6,1,5,-;usb: up\n"), None);
        assert_eq!(
            scrub_log(b"10:00:01.000 14,2,9,-; (2)[412:signal]r1metal: signal: cut"),
            Some(b"10:00:01.000 14,2,9,-;\n".to_vec()),
            "a last line without its end"
        );
    }

    #[test]
    fn crash_copy_blanked() {
        let mut copy =
            b"<6>[ 9.0] (0)[1:init]start\n<6>[ 9.1] (2)[412:signal]r1metal: signal: sent\0\x01\n\xffend".to_vec();
        let length = copy.len();
        assert!(blank_signal(&mut copy, &THREADS));
        assert_eq!(copy.len(), length, "every byte where it was");
        assert_eq!(&copy[..27], b"<6>[ 9.0] (0)[1:init]start\n");
        assert!(copy[27..length - 5].iter().all(|byte| *byte == b' ') && copy[length - 5..] == *b"\n\xffend");
        assert!(!blank_signal(&mut copy, &THREADS), "once");
    }
}
