//! What the writer decides: when it gathers, marks and commits, and whether it opens or closes.

use super::lines::to_stamp_time;
use super::records::{Stream, clock_ahead_us, first_line, record_time_ms, split_records, trusted_mark};
use super::{CRASH, Logging};
use crate::base::text::parse_number;
use crate::base::time::Moment;
use crate::fs::kedump::PARTITION;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// How often the writer gathers; woken for every record it runs thirteen times a second, measured, so only debug records wake it.
pub const GATHER_PERIOD: Duration = Duration::from_secs(1);
const MARK_PERIOD: u32 = (Duration::from_secs(60).as_millis() / GATHER_PERIOD.as_millis()) as u32;
const COMMIT_SHARE: u64 = 2;
const CRASH_STORE: &str = "ramoops.console_size=";
pub const NO_PARTITION: &str = "no data partition";

/// The writer's longest sleep: until the next gathering while the kernel's log is open, without end otherwise.
pub const fn gather_timeout(is_open: bool) -> Option<Duration> {
    if is_open { Some(GATHER_PERIOD) } else { None }
}

const fn is_mark_due(passes: u32) -> bool {
    passes.is_multiple_of(MARK_PERIOD)
}

fn crash_store(cmdline: &str) -> Option<u64> {
    cmdline.split_whitespace().find_map(|word| word.strip_prefix(CRASH_STORE)).and_then(parse_number)
}

fn commit_bytes(cmdline: &str) -> u64 {
    crash_store(cmdline).map_or(0, |size| size / COMMIT_SHARE)
}

const fn should_commit(uncommitted: u64, commit_bytes: u64) -> bool {
    uncommitted > 0 && uncommitted >= commit_bytes
}

/// The day a moment falls on, if the open folder is of another.
pub fn to_new_day(local: u64, open_day: &str) -> Option<String> {
    Some(Moment::from_seconds(local).to_compact_date()).filter(|today| today != open_day)
}

/// The log line of records of the crash store copied; none for no records.
pub fn to_copied_line(count: u32, day: &str) -> Option<String> {
    (count > 0).then(|| format!("logs: {count} new records of the crash store copied to {day}/{CRASH}"))
}

/// The log line of the bootloader's dump copied.
pub fn to_dump_line(day: &str) -> String {
    format!("logs: what the bootloader saved of a dead boot copied to {day}/{CRASH}, {PARTITION} cleared")
}

/// The scrub to do after a pass: a due one, once the settings are read, so that with the logs on what the kernel holds is written first.
pub fn to_scrub_turn(due: Option<u64>, logging: Option<Logging>) -> Option<u64> {
    due.filter(|_| logging.is_some())
}

/// The writer's state from pass to pass.
#[derive(Default)]
pub struct Progress {
    stream: Stream,
    has_begun: bool,
    is_debugging: bool,
    commit_bytes: u64,
    uncommitted: u64,
    passes: u32,
    mark: Option<u64>,
    ahead_us: i64,
}

impl Progress {
    pub fn set_logging(&mut self, logging: Option<Logging>) {
        self.is_debugging = logging == Some(Logging::Debug);
    }

    /// True while a record that waits wakes the writer.
    pub const fn is_debugging(&self) -> bool {
        self.is_debugging
    }

    /// True once the logs have been open since the start.
    pub const fn has_begun(&self) -> bool {
        self.has_begun
    }

    pub const fn note_open(&mut self) {
        (self.has_begun, self.passes) = (true, 0);
    }

    /// Resumes after the record of that number, which is written.
    pub fn resume_after(&mut self, number: Option<u64>) {
        self.stream.resume_after(number);
    }

    /// Takes the kernel's command line, none where it cannot be read.
    pub fn set_commit_bytes(&mut self, cmdline: Option<&str>) {
        self.commit_bytes = cmdline.map_or(0, commit_bytes);
    }

    /// True if a pass makes a clock mark, the kernel's log open or not.
    pub const fn is_mark_due(&self, is_open: bool) -> bool {
        is_open && is_mark_due(self.passes)
    }

    /// Takes a clock mark begun at `before` and done at `after`.
    pub const fn set_mark(&mut self, before: u64, after: u64) {
        self.mark = trusted_mark(before, after);
    }

    /// Counts a pass with an open log.
    pub const fn note_pass(&mut self) {
        self.passes = self.passes.wrapping_add(1);
    }

    /// Adds the records a gathering read to `out`, each dated by the wall and monotonic clocks `now` and stamped at `offset` minutes from UTC.
    pub fn add_records(&mut self, read: &[u8], ends: &[usize], now: (u64, u64), offset: i16, out: &mut Vec<u8>) {
        // A mark made for this gathering is among its records: by it every one of them is stamped.
        let mark = self.mark.take();
        for record in split_records(read, ends) {
            if let Some(ahead) = mark.and_then(|mark| clock_ahead_us(&first_line(record), mark)) {
                self.ahead_us = ahead;
            }
        }
        for record in split_records(read, ends) {
            let at = to_stamp_time(record_time_ms(record, self.ahead_us, now), offset);
            self.stream.add_record(record, at, out);
        }
    }

    /// Adds to the bytes the log has grown by.
    pub const fn add_written(&mut self, bytes: usize) {
        self.uncommitted += bytes as u64;
    }

    /// True if the files are committed after a pass.
    pub const fn is_commit_due(&self) -> bool {
        should_commit(self.uncommitted, self.commit_bytes)
    }

    pub const fn note_commit(&mut self) {
        self.uncommitted = 0;
    }
}

/// True if a run follows an earlier run of the same boot: the boot has a name, and the logs hold it.
pub fn continues_boot(boot: Option<&str>, last_boot: Option<&str>) -> bool {
    boot.is_some() && boot == last_boot
}

/// What the writer does at a pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Turn {
    Open,
    Close,
    // Reports the status to the debug screen, with nothing to open or close.
    Report(Status),
    Stay,
}

/// The writer's switch: a folder that could not be written to is not tried again until the logs are switched off and on.
#[derive(Default)]
pub struct Switch {
    has_failed: bool,
}

impl Switch {
    /// What the writer does now, `is_wanted` none before the settings are read.
    pub fn turn(&mut self, is_wanted: Option<bool>, is_open: bool) -> Turn {
        if is_wanted != Some(true) {
            self.has_failed = false;
        }
        match (is_wanted == Some(true) && !self.has_failed, is_open) {
            (true, false) => Turn::Open,
            (false, true) => Turn::Close,
            (false, false) if !self.has_failed => {
                Turn::Report(if is_wanted.is_none() { Status::Undecided } else { Status::Off })
            }
            _ => Turn::Stay,
        }
    }

    /// Notes a folder that could not be written to.
    pub const fn fail(&mut self) {
        self.has_failed = true;
    }
}

/// How the writing of the logs stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    // The settings an earlier run kept are not read yet.
    Undecided,
    Off,
    // Into the folder of that day.
    On(String),
    // Given up over an error, until the logs are switched off and on.
    Failed(String),
}

impl Status {
    /// The status text on the debug screen and in the log.
    pub fn to_word(&self) -> String {
        match self {
            Status::Undecided => "waiting for the settings".into(),
            Status::Off => "off".into(),
            Status::On(day) => format!("on {day}"),
            Status::Failed(reason) => reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_and_growth() {
        let mut progress = Progress::default();
        assert!(!progress.has_begun() && progress.is_mark_due(true) && !progress.is_mark_due(false));
        progress.note_pass();
        assert!(!progress.is_mark_due(true));
        progress.note_open();
        assert!(progress.has_begun() && progress.is_mark_due(true), "the count begins anew with the folder");
        progress.set_commit_bytes(Some("ramoops.console_size=4096"));
        progress.add_written(2047);
        assert!(!progress.is_commit_due());
        progress.add_written(1);
        assert!(progress.is_commit_due());
        progress.note_commit();
        assert!(!progress.is_commit_due());
        progress.set_commit_bytes(None);
        assert!(!progress.is_commit_due(), "no growth, nothing to commit");
        progress.set_logging(Some(Logging::Debug));
        assert!(progress.is_debugging());
        progress.set_logging(Some(Logging::On));
        assert!(!progress.is_debugging());
    }

    #[test]
    fn mark_dates_records() {
        let mark = 5_000_000;
        let read = b"6,1,5034000,-;a record\n15,2,5034000,-;logs: clock 5000000\n";
        let (first, ends) = (read.iter().position(|byte| *byte == b'\n').unwrap() + 1, [read.len()]);
        let mut progress = Progress::default();
        progress.set_mark(mark, mark + 1);
        let mut out = Vec::new();
        progress.add_records(read, &[first, ends[0]], (100_000, 5_000_000), 0, &mut out);
        assert_eq!(
            alloc::string::String::from_utf8(out).unwrap(),
            "00:01:40.000 -- records 0 to 0 were not kept\n00:01:40.000 6,1,5034000,-;a record\n00:01:40.000 15,2,5034000,-;logs: clock 5000000\n"
        );
    }

    #[test]
    fn new_day_and_scrub_turn() {
        assert_eq!(to_new_day(0, "19700101"), None);
        assert_eq!(to_new_day(86_400, "19700101"), Some("19700102".into()));
        assert_eq!(to_scrub_turn(Some(1), None), None);
        assert_eq!(to_scrub_turn(Some(1), Some(Logging::Off)), Some(1));
        assert_eq!(to_copied_line(0, "20261011"), None);
        assert_eq!(
            to_copied_line(2, "20261011").as_deref(),
            Some("logs: 2 new records of the crash store copied to 20261011/crash")
        );
    }

    #[test]
    fn status_words() {
        assert_eq!(
            [Status::Undecided, Status::Off, Status::On("20261018".into()), Status::Failed("no room".into())]
                .map(|status| status.to_word()),
            ["waiting for the settings", "off", "on 20261018", "no room"]
        );
    }

    #[test]
    fn mark_schedule() {
        assert!(
            is_mark_due(0) && !is_mark_due(1) && !is_mark_due(59) && is_mark_due(60),
            "at the opening and every minute after"
        );
        assert_eq!(GATHER_PERIOD * MARK_PERIOD, Duration::from_secs(60), "a minute of gatherings");
    }

    #[test]
    fn crash_store_size() {
        let cmdline = "console=tty0 ramoops.mem_address=0x47c90000 ramoops.mem_size=0xe0000 ramoops.pmsg_size=0x10000 ramoops.console_size=0x40000 bootopt=64S3,32N2,64N2";
        assert_eq!(crash_store(cmdline), Some(262_144));
        assert_eq!(
            (
                crash_store("ramoops.console_size=4096"),
                crash_store("console=tty0"),
                crash_store(""),
                crash_store("ramoops.console_size=much")
            ),
            (Some(4096), None, None, None)
        );
    }

    #[test]
    fn commit_schedule() {
        assert_eq!(commit_bytes("console=tty0 ramoops.console_size=0x40000"), 131_072);
        assert_eq!(commit_bytes("console=tty0"), 0, "without a store every pass that grew the log commits");
        assert!(
            should_commit(131_072, 131_072) && !should_commit(131_071, 131_072),
            "once it has grown as far as the store asks"
        );
        assert!(
            should_commit(1, 0) && !should_commit(0, 0),
            "and without a store after every pass that grew it, none that did not"
        );
    }

    #[test]
    fn switch_turns() {
        let mut switch = Switch::default();
        assert_eq!(switch.turn(None, false), Turn::Report(Status::Undecided), "the settings are not read yet");
        assert_eq!(switch.turn(Some(false), false), Turn::Report(Status::Off));
        assert_eq!((switch.turn(Some(true), false), switch.turn(Some(true), true)), (Turn::Open, Turn::Stay));
        assert_eq!(switch.turn(Some(false), true), Turn::Close);
        switch.fail();
        assert_eq!(
            (switch.turn(Some(true), false), switch.turn(Some(true), false)),
            (Turn::Stay, Turn::Stay),
            "a folder that failed is not tried while the logs stay on"
        );
        assert_eq!(switch.turn(Some(false), false), Turn::Report(Status::Off), "switched off");
        assert_eq!(switch.turn(Some(true), false), Turn::Open, "and on, it is tried again");
    }

    #[test]
    fn boot_continuation() {
        assert!(continues_boot(Some("1f2e"), Some("1f2e")));
        assert!(
            !continues_boot(Some("1f2e"), Some("9a0b"))
                && !continues_boot(Some("1f2e"), None)
                && !continues_boot(None, None),
            "another boot's, none kept, or a boot without a name"
        );
    }

    #[test]
    fn gather_timeouts() {
        assert_eq!(gather_timeout(true), Some(Duration::from_secs(1)));
        assert_eq!(gather_timeout(false), None, "off, nothing wakes it but a word from the firmware");
    }
}
