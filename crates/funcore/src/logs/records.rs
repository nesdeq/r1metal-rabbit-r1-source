//! Kernel records as the writer gathers them: their fields, the run of their numbers, the clock marks that date them.

use super::lines::{NOTE, write_stamp};
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const CLOCK_MARK: &str = "logs: clock ";
const MARK_LEVEL: u32 = 15;
const MICROS_PER_MILLI: u64 = 1000;
const MARK_LIMIT_US: u64 = MICROS_PER_MILLI;
const LAST_RECORDS: u64 = 2;

pub(super) struct Record<'a> {
    level: u32,
    number: u64,
    micros: u64,
    text: &'a str,
}

pub(super) fn parse_record(record: &str) -> Option<Record<'_>> {
    let (head, text) = record.split_once(';')?;
    let mut fields = head.split(',');
    let (level, number, micros) =
        (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
    Some(Record { level, number, micros, text })
}

fn record_number(record: &str) -> Option<u64> {
    parse_record(record).map(|record| record.number)
}

fn record_micros(record: &str) -> Option<u64> {
    parse_record(record).map(|record| record.micros)
}

#[derive(Default)]
pub(super) struct Stream {
    last: Option<u64>,
    skip: Option<u64>,
}

impl Stream {
    pub(super) fn resume_after(&mut self, number: Option<u64>) {
        (self.last, self.skip) = (number.or(self.last), number);
    }

    pub(super) fn add_record(&mut self, record: &[u8], (local, millis): (u64, u32), out: &mut Vec<u8>) {
        let number = record_number(&first_line(record));
        if number.zip(self.skip).is_some_and(|(number, skip)| number <= skip) {
            return;
        }
        let mut stamp = String::new();
        write_stamp(&mut stamp, local, millis);
        if let Some(number) = number {
            let expected = self.last.map_or(0, |last| last + 1);
            if number > expected {
                out.extend_from_slice(
                    format!("{stamp}{NOTE} records {expected} to {} were not kept\n", number - 1).as_bytes(),
                );
            }
            self.last = Some(number);
        }
        out.extend_from_slice(stamp.as_bytes());
        out.extend_from_slice(record);
        if !record.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
}

pub(super) fn split_records<'a>(read: &'a [u8], ends: &'a [usize]) -> impl Iterator<Item = &'a [u8]> {
    ends.iter().scan(0, |start, end| Some(&read[core::mem::replace(start, *end)..*end]))
}

pub(super) fn first_line(record: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(record.split(|byte| *byte == b'\n').next().unwrap_or_default())
}

pub(super) fn record_time_ms(record: &[u8], ahead_us: i64, (wall_ms, monotonic_us): (u64, u64)) -> u64 {
    let age_us =
        record_micros(&first_line(record)).map_or(0, |micros| (monotonic_us as i64 + ahead_us - micros as i64).max(0));
    wall_ms.saturating_sub(age_us as u64 / MICROS_PER_MILLI)
}

/// The text of a clock mark: the monotonic microseconds that date the kernel's log clock against that one.
pub fn clock_mark(monotonic_micros: u64) -> String {
    format!("{CLOCK_MARK}{monotonic_micros}")
}

fn mark_micros(record: &str) -> Option<u64> {
    let record = parse_record(record)?;
    let (_, micros) = record.text.split_once(CLOCK_MARK)?;
    micros.trim_end().parse().ok().filter(|_| record.level == MARK_LEVEL)
}

pub(super) const fn trusted_mark(before: u64, after: u64) -> Option<u64> {
    if after.saturating_sub(before) <= MARK_LIMIT_US { Some(before) } else { None }
}

pub(super) fn clock_ahead_us(line: &str, mark: u64) -> Option<i64> {
    record_micros(line).filter(|_| mark_micros(line) == Some(mark)).map(|micros| micros as i64 - mark as i64)
}

/// The offset of the end of a `log` file that holds its last record, by the largest record.
pub const fn end_of_log(size: u64, record_bytes: usize) -> u64 {
    size.saturating_sub(LAST_RECORDS * record_bytes as u64)
}

/// The number of the last kernel record in the end of a `log` file read from the offset `start`: only an end that is the whole file begins with a line.
pub fn last_number(end: &[u8], start: u64) -> Option<u64> {
    let text = String::from_utf8_lossy(end);
    let mut lines = text.lines();
    let first = lines.next().filter(|_| start == 0);
    lines.rev().chain(first).find_map(|line| record_number(line.split_once(' ')?.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_numbers() {
        assert_eq!(
            record_number("3,130401,7564029328,-; (4)[4294:kworker/u16:3][HYN]hyn_esd_check_func: Enter"),
            Some(130_401)
        );
        assert_eq!(
            record_number("14,134493,7769957024,-; (6)[356:battery]r1metal: battery: 73% Charging"),
            Some(134_493)
        );
        assert_eq!(record_number("6,0,0,-,caller=T1;Booting Linux"), Some(0));
        for other in [
            "",
            " SUBSYSTEM=platform",
            "6,130405;text",
            "six,130405,7564292159,-;text",
            "6,130405,7564292159,- text",
            "6,-1,7564292159,-;text",
            "boot 1f2e; 3,4,5",
        ] {
            assert_eq!(record_number(other), None, "{other:?}");
        }
    }

    #[test]
    fn record_times_and_marks() {
        assert_eq!(
            record_micros("3,130401,7564029328,-; (4)[4294:kworker/u16:3][HYN]hyn_esd_check_func: Enter"),
            Some(7_564_029_328)
        );
        assert_eq!(
            (record_micros("6,0,0,-,caller=T1;Booting Linux"), record_micros(" SUBSYSTEM=platform"), record_micros("")),
            (Some(0), None, None)
        );
        assert_eq!(clock_mark(5_183_359_000), "logs: clock 5183359000");
        let mark = "15,9001,5183393945,-; (6)[356:logs]r1metal: logs: clock 5183359000";
        assert_eq!((mark_micros(mark), record_micros(mark)), (Some(5_183_359_000), Some(5_183_393_945)));
        assert_eq!(mark_micros(&format!("{mark}\n")), Some(5_183_359_000), "with the line end /dev/kmsg hands out");
        for other in [
            "14,9002,5183393945,-; (6)[363:radio]r1metal: wifi: heard logs: clock 5183359000",
            "15,9003,5183393945,-; (6)[356:logs]r1metal: logs: clock soon",
            "15,9004,5183393945,-; (6)[356:logs]r1metal: logs: on",
            "logs: clock 5183359000",
        ] {
            assert_eq!(mark_micros(other), None, "{other:?}");
        }
    }

    #[test]
    fn last_numbers() {
        let end = b"19:29:42.031 3,130401,7564029328,-; one\n19:29:42.292 6,130405,7564292159,-; two\n SUBSYSTEM=platform\n DEVICE=+platform:mt635x-auxadc\n";
        assert_eq!(last_number(end, 0), Some(130_405), "the lines that follow a record are none");
        assert_eq!(last_number(&end[..40], 0), Some(130_401));
        assert_eq!(
            last_number(
                b"19:29:42.031 3,130401,7564029328,-; one\n19:30:00.000 130402 records before this one are gone\n",
                0
            ),
            Some(130_401)
        );
        assert_eq!(last_number(&end[3..42], 3), None, "a line cut off at its beginning is not trusted");
        assert_eq!(last_number(&end[3..], 3), Some(130_405));
        assert_eq!(
            (last_number(b"", 0), last_number(b"\n\n", 0), last_number(&[0xff, 0xfe, b'\n'], 0)),
            (None, None, None)
        );
    }

    #[test]
    fn stamped_records_and_gaps() {
        let (mut stream, mut out) = (Stream::default(), Vec::new());
        stream.add_record(b"6,0,0,-;Booting Linux\n", (0, 5), &mut out);
        stream.add_record(b"6,1,12,-;second\n SUBSYSTEM=platform\n", (0, 6), &mut out);
        stream.add_record(b"4,7,90,-;eighth", (61, 0), &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "00:00:00.005 6,0,0,-;Booting Linux\n00:00:00.006 6,1,12,-;second\n SUBSYSTEM=platform\n00:01:01.000 -- records 2 to 6 were not kept\n00:01:01.000 4,7,90,-;eighth\n"
        );
        assert_eq!(last_number(text.as_bytes(), 0), Some(7), "what is written tells where it ends");
        let (mut late, mut out) = (Stream::default(), Vec::new());
        late.add_record(b"6,130401,7564029328,-;first that is held\n", (0, 0), &mut out);
        assert!(
            String::from_utf8(out).unwrap().starts_with("00:00:00.000 -- records 0 to 130400 were not kept\n"),
            "a log that begins late says what is missing"
        );
    }

    #[test]
    fn resumed_run() {
        let (mut stream, mut out) = (Stream::default(), Vec::new());
        stream.resume_after(Some(41));
        for record in [&b"6,40,1,-;old\n"[..], b"6,41,2,-;old\n", b"6,42,3,-;new\n", b"no record at all\n"] {
            stream.add_record(record, (0, 0), &mut out);
        }
        assert_eq!(String::from_utf8(out).unwrap(), "00:00:00.000 6,42,3,-;new\n00:00:00.000 no record at all\n");
        let mut unwritten = Stream::default();
        unwritten.resume_after(None);
        assert_eq!((unwritten.last, unwritten.skip), (None, None), "nothing written before: everything is taken");
    }

    #[test]
    fn record_dates() {
        let now = (100_000, 5_966_000);
        assert_eq!(record_time_ms(b"6,10,5000000,-;made a second ago\n", 34_000, now), 99_000);
        assert_eq!(record_time_ms(b"6,11,5999999,-;made just now\n", 34_000, now), 100_000);
        assert_eq!(
            record_time_ms(b"6,12,9000000,-;its clock is further ahead than the last mark said\n", 34_000, now),
            100_000,
            "no record is made after it is gathered"
        );
        assert_eq!(
            record_time_ms(b" SUBSYSTEM=platform\n", 34_000, now),
            100_000,
            "what is no record is dated as gathered"
        );
        let read = b"6,1,5,-;one\n SUBSYSTEM=platform\n15,2,9,-;two\n";
        let both: Vec<&[u8]> = split_records(read, &[32, 45]).collect();
        assert_eq!(both, [&read[..32], &read[32..]], "a gathering's records come apart where each ended");
        assert_eq!(first_line(both[0]), "6,1,5,-;one", "the lines that follow a record's first are none of its head");
    }

    #[test]
    fn trusted_marks() {
        assert_eq!(
            [trusted_mark(500, 500), trusted_mark(500, 1500), trusted_mark(500, 1501)],
            [Some(500), Some(500), None]
        );
        let mark = "15,9001,5183393945,-; (6)[356:logs]r1metal: logs: clock 5183359000";
        assert_eq!(clock_ahead_us(mark, 5_183_359_000), Some(34_945));
        assert_eq!(
            (
                clock_ahead_us(mark, 5_183_359_001),
                clock_ahead_us("6,9002,5183393945,-;logs: clock 5183359000", 5_183_359_000)
            ),
            (None, None),
            "a mark this gathering did not make, and a line that only quotes one"
        );
    }
}
