use crate::sys::devnodes;
use r1_funcore::fs::kedump;
use r1_funcore::hw::slots::{self, Mark};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

const LOG_STORE: &str = "/proc/pl_lk";
const BOOTED: &[u8] = b"0";

/// Marks the slot on the kernel command line as booted.
pub fn mark_booted(cmdline: &str) -> io::Result<Mark> {
    let suffix = slots::suffix(cmdline).ok_or_else(|| io::Error::other(slots::NO_SUFFIX))?;
    let slot = slots::index(suffix).ok_or_else(|| io::Error::other(slots::to_unknown_text(suffix)))?;
    let file = OpenOptions::new().read(true).write(true).open(devnodes::partition_node(slots::MISC_PARTITION)?)?;
    let mut block = [0u8; slots::CONTROL_SIZE];
    file.read_exact_at(&mut block, slots::CONTROL_OFFSET)?;
    let mark = slots::mark_successful(&mut block, slot).map_err(|e| io::Error::other(e.message()))?;
    if mark == Mark::Dirty {
        file.write_all_at(&block, slots::CONTROL_OFFSET)?;
        file.sync_all()?;
    }
    Ok(mark)
}

/// Tells the log store the boot came up; it records that in expdb, whose node is made first.
pub fn tell_booted() -> io::Result<()> {
    devnodes::partition_node(kedump::PARTITION)?;
    fs::write(LOG_STORE, BOOTED)
}
