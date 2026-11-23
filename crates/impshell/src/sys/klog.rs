use super::lock;
use r1_funcore::logs::Logging;
use r1_funcore::logs::kernel::{KernelLog, Level, SILENT_SWITCH, Step, to_record};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, OnceLock};

const KMSG: &str = "/dev/kmsg";
const RATE_LIMIT: &str = "/proc/sys/kernel/printk_devkmsg";
const UNLIMITED: &str = "on\n";
/// CONSOLE_EXT_LOG_MAX, the largest record of /dev/kmsg.
pub const MAX_RECORD: usize = 8192;
const SYSLOG_ACTION_CLEAR: libc::c_int = 5;

static SINK: OnceLock<File> = OnceLock::new();
static STATE: Mutex<KernelLog> = Mutex::new(KernelLog::new());

pub fn init() {
    if let Ok(file) = OpenOptions::new().write(true).open(KMSG) {
        let _ = SINK.set(file);
    }
}

/// Removes the kernel's limit of ten records in five seconds, past which it drops a record and reports it written.
pub fn remove_rate_limit() -> io::Result<()> {
    fs::write(RATE_LIMIT, UNLIMITED)
}

/// Silences this process alone, before the kernel's switch is reachable.
pub fn mute() {
    lock(&STATE).set_silent(true);
}

/// Sets the kernel's and this process's logging.
pub fn set(logging: Logging) -> io::Result<()> {
    let mut result = Ok(());
    for step in logging.to_steps() {
        match step {
            Step::Write(switch, text) => result = result.and_then(|()| fs::write(switch, text)),
            Step::Silence(is_silent) => lock(&STATE).set_silent(is_silent),
        }
    }
    result
}

/// Silences this process while the kernel's switch stores no record.
pub fn follow_kernel() {
    if let Ok(text) = fs::read_to_string(SILENT_SWITCH) {
        lock(&STATE).follow(&text);
    }
}

/// True while nothing is logged.
pub fn is_silent() -> bool {
    lock(&STATE).is_silent()
}

fn emit(level: Level, line: &str) {
    if let Some(mut sink) = SINK.get() {
        let _ = sink.write_all(to_record(level, line).as_bytes());
    }
}

/// Writes a kernel log record and queues the line for the console; nothing while silent.
pub fn write(line: String) {
    if is_silent() {
        return;
    }
    emit(Level::Info, &line);
    lock(&STATE).add(line);
}

/// Writes a debug record, which no console takes; nothing while silent.
pub fn write_debug(line: &str) {
    if !is_silent() {
        emit(Level::Debug, line);
    }
}

pub fn drain() -> Vec<String> {
    lock(&STATE).take()
}

pub fn has_pending() -> bool {
    lock(&STATE).has_pending()
}

/// Clears the kernel's records, so a crash dump pstore keeps carries none of them.
pub fn clear() -> io::Result<()> {
    super::check(unsafe { libc::klogctl(SYSLOG_ACTION_CLEAR, std::ptr::null_mut(), 0) }).map(drop)
}

/// Opens the kernel's log for non-blocking reads.
pub fn reader() -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(KMSG)
}

/// Hands `handle` every record `kmsg` holds, in order; one overwritten before its read is skipped.
pub fn records(kmsg: &mut File, mut handle: impl FnMut(&[u8])) -> io::Result<()> {
    let mut record = [0u8; MAX_RECORD];
    loop {
        match kmsg.read(&mut record) {
            Ok(count) => handle(&record[..count]),
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::BrokenPipe) => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// Writes panics to the kernel log before the process aborts.
pub fn log_panics() {
    std::panic::set_hook(Box::new(|info| write(format!("panic: {info}"))));
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::sys::klog::write(format!($($arg)*))
    };
}
