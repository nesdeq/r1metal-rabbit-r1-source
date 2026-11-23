//! The logs on the data partition: a day's folder, its files and archive.

mod crashes;
mod days;
pub mod kernel;
pub mod lines;
mod queue;
pub mod records;
pub mod scrub;
pub mod snapshot;
mod state;
pub mod writer;

pub use crashes::{Crashes, to_changed_list};
use days::Days;
pub use queue::{Lines, Queue};
pub use state::State;

use crate::base::file_names::numbered_file;
use crate::base::text::{OFF, ON};
use crate::base::time::Moment;
use crate::base::time::stamp::is_compact_day;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use kernel::Step;

/// The days' folders and archives in the private directory.
pub const DIRECTORY: &str = ".r1metal/logs";
/// `DIRECTORY` on a device, for the host that copies it.
pub const ON_DEVICE: &str = "/data/.r1metal/logs";
/// The file that names the boot the kernel's log was last read in.
pub const BOOT: &str = "boot";
/// The file that lists the crash store's records copied.
pub const CRASH_LIST: &str = "crashes";
/// The file that names the boot Signal's traces were taken out in, kept until a later boot's copies of a crash are taken.
pub const SCRUBBED: &str = "scrubbed";
/// A day's file of the kernel's log with the firmware's lines.
pub const LOG: &str = "log";
const INPUT: &str = "input";
pub const FRAMES: &str = "frames";
pub const POWER: &str = "power";
const STATE: &str = "state";
/// A day's folder for the crash store's records.
pub const CRASH: &str = "crash";
/// The name of the copy of the bootloader's dump.
pub const DUMP_COPY: &str = "expdb";
/// The name of the copy of the reboot record.
pub const RECORD_COPY: &str = "reboot-reason";
const ARCHIVE: &str = "tar.gz";
const WORD_DEBUG: &str = "debug";
/// The log line of a way down that came before the logs were written.
pub const UNSETTLED_LINE: &str = "logs: not everything was written in the time a way down waits";

/// How much is logged, the kernel's and the app's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Logging {
    Off,
    On,
    Debug,
}

impl Logging {
    /// Its word in the settings file and on the console.
    pub const fn to_word(self) -> &'static str {
        match self {
            Logging::Off => OFF,
            Logging::On => ON,
            Logging::Debug => WORD_DEBUG,
        }
    }

    pub fn parse(word: &str) -> Option<Logging> {
        match word.trim() {
            OFF => Some(Logging::Off),
            ON => Some(Logging::On),
            WORD_DEBUG => Some(Logging::Debug),
            _ => None,
        }
    }

    /// The logging its switch turns to next.
    pub const fn next(self) -> Logging {
        match self {
            Logging::On => Logging::Debug,
            Logging::Debug => Logging::Off,
            Logging::Off => Logging::On,
        }
    }

    /// True if anything is logged.
    pub const fn is_on(self) -> bool {
        !matches!(self, Logging::Off)
    }

    /// The steps that set it, the kernel's and this process's.
    pub const fn to_steps(self) -> [Step; 3] {
        kernel::to_steps(self.is_on(), matches!(self, Logging::Debug))
    }

    /// The kernel parameter for it, so that it holds before the app sets the switches; none for on.
    pub const fn to_parameter(self) -> Option<&'static str> {
        kernel::to_parameter(self.is_on(), matches!(self, Logging::Debug))
    }

    /// The log line of it set.
    pub fn to_line(self) -> String {
        format!("logging: {}", self.to_word())
    }
}

/// True if a frame's record has a reader: debug logging alone makes one, where the firmware's lines have a reader.
pub const fn are_frames_read(logging: Option<Logging>, are_lines_read: bool) -> bool {
    are_lines_read && matches!(logging, Some(Logging::Debug))
}

/// The files of a day's folder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Log,
    Frames,
    Input,
    Power,
    State,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Log, Kind::Frames, Kind::Input, Kind::Power, Kind::State];

    pub const fn name(self) -> &'static str {
        match self {
            Kind::Log => LOG,
            Kind::Frames => FRAMES,
            Kind::Input => INPUT,
            Kind::Power => POWER,
            Kind::State => STATE,
        }
    }
}

/// The files of a day's folder that take the lines the threads hand over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineFile {
    Frames,
    Input,
    Power,
}

impl LineFile {
    pub const ALL: [LineFile; 3] = [LineFile::Frames, LineFile::Input, LineFile::Power];

    pub const fn to_kind(self) -> Kind {
        match self {
            LineFile::Frames => Kind::Frames,
            LineFile::Input => Kind::Input,
            LineFile::Power => Kind::Power,
        }
    }
}

/// The name of a day's archive; `count` tells it from the ones that day already has.
pub fn archive_name(day: &str, count: u32) -> String {
    numbered_file(day, ARCHIVE, count)
}

fn is_archive_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(ARCHIVE).and_then(|stem| stem.strip_suffix('.')) else {
        return false;
    };
    match stem.split_once('_') {
        Some((day, count)) => count.parse::<u32>().is_ok() && is_compact_day(day),
        None => is_compact_day(stem),
    }
}

/// The files of a day in the order an archive holds them, by their names.
pub fn to_tape_order<P: Ord>(mut files: Vec<(String, P)>) -> Vec<(String, P)> {
    files.sort();
    files
}

/// The log line of a day packed.
pub fn to_packed_line(day: &str, name: &str, total: u64, packed: u64, ms: u128) -> String {
    format!("logs: {day} packed into {name}, {total} bytes as {packed}, in {ms} ms")
}

/// The log line of an archive that cannot be read.
pub fn to_unreadable_line(name: &str) -> String {
    format!("logs: {name} cannot be read as an archive, and goes")
}

/// The name of a copy in a day's crash folder: the time of day it was made, then its own.
pub fn crash_copy_name(local: u64, name: &str) -> String {
    let moment = Moment::from_seconds(local);
    format!("{:02}{:02}{:02}-{name}", moment.hour, moment.minute, moment.second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names() {
        assert_eq!(
            [archive_name("20261011", 0), archive_name("20261011", 1), archive_name("20261011", 12)],
            ["20261011.tar.gz", "20261011_1.tar.gz", "20261011_12.tar.gz"]
        );
        assert!(!is_compact_day(&archive_name("20261011", 0)), "an archive is never taken for a folder");
        assert!([archive_name("20261011", 0), archive_name("20261011", 12)].iter().all(|name| is_archive_name(name)));
        for name in ["20261011", "20261011.tar.gz.new", "20261011_x.tar.gz", "20260230.tar.gz", "boot", ".tar.gz"] {
            assert!(!is_archive_name(name), "{name:?} is no day's archive");
        }
    }

    #[test]
    fn crash_copy_names() {
        assert_eq!(crash_copy_name(1_791_697_759, "console-ramoops-0"), "054919-console-ramoops-0");
    }

    #[test]
    fn kind_names() {
        assert_eq!(Kind::ALL.map(Kind::name), [LOG, FRAMES, INPUT, POWER, STATE]);
        assert_eq!(
            LineFile::ALL.map(|file| file.to_kind().name()),
            [FRAMES, INPUT, POWER],
            "the files of lines, in the order a pass writes them"
        );
    }

    #[test]
    fn frame_readers() {
        assert!(are_frames_read(Some(Logging::Debug), true), "debug logging with a reader");
        for logging in [None, Some(Logging::Off), Some(Logging::On)] {
            assert!(!are_frames_read(logging, true), "{logging:?} makes no frame's record");
        }
        assert!(!are_frames_read(Some(Logging::Debug), false), "nor does debug without a reader");
    }

    #[test]
    fn logging_switch_order() {
        assert_eq!(
            [Logging::On, Logging::Debug, Logging::Off].map(Logging::next),
            [Logging::Debug, Logging::Off, Logging::On]
        );
        assert!(Logging::On.is_on() && Logging::Debug.is_on() && !Logging::Off.is_on());
    }

    #[test]
    fn logging_parameters() {
        assert_eq!(
            [Logging::Off, Logging::On, Logging::Debug].map(Logging::to_parameter),
            [Some("printk.silent=1"), None, Some("printk.debug=1")],
            "off stores no record from the kernel's first line on, and debug stores its debug records too"
        );
    }
}
