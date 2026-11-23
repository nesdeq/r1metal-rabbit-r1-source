use crate::sys;
use r1_funcore::hw::uevent::{self, NAME_KEY, Outcome, parse_device_number};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DEV: &str = "/dev";
const CHAR_INDEX: &str = "/sys/dev/char";
const BLOCK_INDEX: &str = "/sys/dev/block";

/// Creates /dev/`name` unless it exists; true if this call made the node.
pub fn make_node(name: &str, kind: libc::mode_t, major: u32, minor: u32) -> io::Result<bool> {
    let node = Path::new(DEV).join(name);
    if node.symlink_metadata().is_ok() {
        return Ok(false);
    }
    if let Some(parent) = node.parent() {
        fs::create_dir_all(parent)?;
    }
    match sys::mknod(&node, kind, major, minor) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        outcome => outcome.map(|()| true),
    }
}

fn create(entry: &Path, kind: libc::mode_t) -> io::Result<bool> {
    let Some((major, minor)) = entry.file_name().and_then(|name| name.to_str()).and_then(parse_device_number) else {
        return Ok(false);
    };
    let uevent = fs::read_to_string(entry.join("uevent"))?;
    let Some(name) = uevent::value(&uevent, NAME_KEY) else {
        return Ok(false);
    };
    make_node(name, kind, major, minor)
}

fn scan(index: &str, kind: libc::mode_t, outcome: &mut Outcome) {
    for entry in fs::read_dir(index).into_iter().flatten().flatten() {
        outcome.add(create(&entry.path(), kind));
    }
}

/// Creates a node for every device the kernel lists; later ones come as uevents.
pub fn coldplug() -> Outcome {
    let mut outcome = Outcome::default();
    scan(CHAR_INDEX, libc::S_IFCHR, &mut outcome);
    scan(BLOCK_INDEX, libc::S_IFBLK, &mut outcome);
    outcome
}

/// The node of the boot device's partition `name`, made if its event has not come yet.
pub fn partition_node(name: &str) -> io::Result<PathBuf> {
    for entry in fs::read_dir(BLOCK_INDEX)?.flatten() {
        let Ok(uevent) = fs::read_to_string(entry.path().join("uevent")) else {
            continue;
        };
        let (number, link) = (entry.file_name(), fs::read_link(entry.path()).unwrap_or_default());
        let (number, link) = (number.to_str().unwrap_or_default(), link.to_str().unwrap_or_default());
        if let Some((device, major, minor)) = uevent::partition_node(name, number, &uevent, link) {
            make_node(device, libc::S_IFBLK, major, minor)?;
            return Ok(Path::new(DEV).join(device));
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, uevent::to_no_partition_text(name)))
}
