//! The kernel log's switches, the form of a firmware record, and the lines that wait for a reader.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const SILENT_SWITCH: &str = "/sys/module/printk/parameters/silent";
const DEBUG_SWITCH: &str = "/sys/module/printk/parameters/debug";
const SILENT_PARAMETER: &str = "printk.silent=1";
const DEBUG_PARAMETER: &str = "printk.debug=1";
const SET: &str = "Y\n";
const UNSET: &str = "N\n";
const INFO_LEVEL: &str = "<6>";
const DEBUG_LEVEL: &str = "<7>";
const TAG: &str = "r1metal";

/// True if a firmware line has a reader: nothing is silent, and a console or the log files take it.
pub const fn are_lines_read(is_silent: bool, has_console: bool, is_written: bool) -> bool {
    !is_silent && (has_console || is_written)
}

/// A record's level among the kernel's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Debug,
}

/// A line as the kernel's log takes it.
pub fn to_record(level: Level, line: &str) -> String {
    let level = match level {
        Level::Info => INFO_LEVEL,
        Level::Debug => DEBUG_LEVEL,
    };
    format!("{level}{TAG}: {line}\n")
}

/// A step of a change of how much is logged: a switch of the kernel's, or this process's own silence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Write(&'static str, &'static str),
    Silence(bool),
}

// Silence comes first and goes last, this process's around the kernel's, so that the drivers' debug sites turn once and no line goes to a kernel that stores none.
pub(super) const fn to_steps(is_on: bool, is_debug: bool) -> [Step; 3] {
    let debug = Step::Write(DEBUG_SWITCH, if is_debug { SET } else { UNSET });
    if is_on {
        [debug, Step::Write(SILENT_SWITCH, UNSET), Step::Silence(false)]
    } else {
        [Step::Silence(true), Step::Write(SILENT_SWITCH, SET), debug]
    }
}

pub(super) const fn to_parameter(is_on: bool, is_debug: bool) -> Option<&'static str> {
    match (is_on, is_debug) {
        (false, _) => Some(SILENT_PARAMETER),
        (true, true) => Some(DEBUG_PARAMETER),
        (true, false) => None,
    }
}

/// The lines that wait for a reader, and whether this process logs.
#[derive(Debug, Default)]
pub struct KernelLog {
    pending: Vec<String>,
    is_silent: bool,
}

impl KernelLog {
    pub const fn new() -> KernelLog {
        KernelLog { pending: Vec::new(), is_silent: false }
    }

    pub const fn is_silent(&self) -> bool {
        self.is_silent
    }

    pub const fn set_silent(&mut self, is_silent: bool) {
        self.is_silent = is_silent;
    }

    /// Follows what the kernel's silence switch reads.
    pub fn follow(&mut self, switch: &str) {
        self.is_silent = switch == SET;
    }

    pub fn add(&mut self, line: String) {
        self.pending.push(line);
    }

    pub fn take(&mut self) -> Vec<String> {
        core::mem::take(&mut self.pending)
    }

    pub const fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_order() {
        let is_silence = |step: &Step| matches!(step, Step::Silence(_));
        assert!(is_silence(&to_steps(false, false)[0]) && is_silence(&to_steps(true, true)[2]));
        assert_eq!(to_steps(true, true)[0], Step::Write(DEBUG_SWITCH, SET));
        assert_eq!(to_record(Level::Debug, "up"), "<7>r1metal: up\n");
    }

    #[test]
    fn parameters_of_switches() {
        for (switch, parameter) in [(SILENT_SWITCH, SILENT_PARAMETER), (DEBUG_SWITCH, DEBUG_PARAMETER)] {
            let name = switch.rsplit_once('/').map(|(_, name)| name).unwrap_or_default();
            assert_eq!(parameter, format!("printk.{name}=1"), "the command line sets the switch its file sets");
        }
        assert_eq!(
            [
                to_parameter(false, false),
                to_parameter(false, true),
                to_parameter(true, false),
                to_parameter(true, true)
            ],
            [Some(SILENT_PARAMETER), Some(SILENT_PARAMETER), None, Some(DEBUG_PARAMETER)],
            "silence goes before everything, and logging as the kernel starts with needs no parameter"
        );
    }

    #[test]
    fn follows_kernel_switch() {
        let mut log = KernelLog::default();
        assert!(!log.is_silent() && !log.has_pending(), "at first it logs, and nothing waits");
        log.follow(SET);
        assert!(log.is_silent(), "silent while the kernel stores no record");
        log.follow(UNSET);
        assert!(!log.is_silent(), "and no longer once it does");
        log.add("one".into());
        assert_eq!((log.has_pending(), log.take(), log.has_pending()), (true, alloc::vec!["one".into()], false));
    }

    #[test]
    fn line_readers() {
        assert!(are_lines_read(false, true, false) && are_lines_read(false, false, true));
        assert!(!are_lines_read(true, true, true) && !are_lines_read(false, false, false));
    }
}
