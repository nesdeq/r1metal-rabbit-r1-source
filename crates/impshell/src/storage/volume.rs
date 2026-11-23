use super::{Shared, restore, safe};
use crate::sys::{self, devnodes};
use crate::{crypto, log};
use r1_funcore::base::code::{UUID_RANDOM, to_uuid};
use r1_funcore::domain::partition::PRIVATE_DIRECTORIES;
use r1_funcore::fs::f2fs::{self, BLOCK_SIZE, BLOCKS_PER_SEGMENT, Identity, Request, UUID_SIZE};
use r1_funcore::storage::progress::{
    ENCRYPTING_LINE, LOST_LABEL, NAMELESS_NODE, Progress, UNREADABLE_NODE, identify, to_foreign_line, to_formatted_line,
};
use r1_funcore::storage::{DIRECTORIES, FILESYSTEM, LABEL, MOUNT_POINT, PARTITION, paths};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::Instant;

const MOUNTS: &str = "/proc/mounts";
const BLOCK_DEVICES: &str = "/sys/class/block";
const BLKDISCARD: u32 = sys::ioc(0, 0x12, 119, 0);
const BLOCK: u64 = BLOCK_SIZE as u64;
const VERSION_BYTES: usize = 4;
const SAFE: &str = "the safe";
const RESTORE_FILES: &str = "a restore's files";

fn read_identity(device: &File) -> io::Result<Option<Identity>> {
    for number in f2fs::SUPERBLOCKS {
        let mut block = [0u8; BLOCK_SIZE];
        device.read_exact_at(&mut block, number * BLOCK)?;
        if let Some(identity) = identify(&block) {
            return Ok(Some(identity));
        }
    }
    Ok(None)
}

fn zero(device: &File, runs: &[Range<u64>]) -> io::Result<()> {
    let zeros = vec![0u8; BLOCKS_PER_SEGMENT as usize * BLOCK_SIZE];
    for (start, blocks) in f2fs::zero_writes(runs) {
        device.write_all_at(&zeros[..blocks as usize * BLOCK_SIZE], start * BLOCK)?;
    }
    Ok(())
}

fn format_device(mut device: &File, node: &Path) -> io::Result<()> {
    let start = Instant::now();
    let bytes = device.seek(SeekFrom::End(0))?;
    let name = node.file_name().ok_or_else(|| io::Error::other(NAMELESS_NODE))?;
    let start_sector = sys::read_number(Path::new(BLOCK_DEVICES).join(name).join("start"))?;
    // The device is told that nothing of it is needed any more.
    let mut range = [0u64, bytes];
    let is_trimmed = unsafe { sys::ioctl(device.as_raw_fd(), BLKDISCARD, &mut range) }
        .inspect_err(|e| log!("data: discard: {e}"))
        .is_ok();
    let discard_time = start.elapsed();

    let (id, version) = crypto::random::<UUID_SIZE>()
        .zip(crypto::random::<VERSION_BYTES>())
        .ok_or_else(|| io::Error::other("no random bytes for the filesystem's id"))?;
    let request = Request {
        bytes,
        start_sector,
        label: LABEL,
        uuid: to_uuid(id, UUID_RANDOM),
        checkpoint: f2fs::first_checkpoint(u32::from_le_bytes(version)),
        kernel: &fs::read_to_string(sys::KERNEL_VERSION)?,
        time: sys::realtime_seconds(),
        is_trimmed,
    };
    let image = f2fs::format(&request).map_err(|e| io::Error::other(e.message()))?;
    zero(device, &image.zero_runs)?;
    for (number, block) in &image.blocks {
        device.write_all_at(block, number * BLOCK)?;
    }
    device.sync_all()?;
    for number in f2fs::SUPERBLOCKS {
        device.write_all_at(&image.superblock, number * BLOCK)?;
    }
    device.sync_all()?;
    log!("{}", to_formatted_line(bytes, start.elapsed().as_millis(), discard_time.as_millis()));
    Ok(())
}

fn allow_encryption(device: &File) -> io::Result<()> {
    let mut is_changed = false;
    for number in f2fs::SUPERBLOCKS {
        let mut block = [0u8; BLOCK_SIZE];
        device.read_exact_at(&mut block, number * BLOCK)?;
        if f2fs::allow_encryption(&mut block) == Some(true) {
            device.write_all_at(&block, number * BLOCK)?;
            is_changed = true;
        }
    }
    if is_changed {
        device.sync_all()?;
        log!("{ENCRYPTING_LINE}");
    }
    Ok(())
}

fn bring_up(node: &Path, shared: &Shared) -> io::Result<Identity> {
    let mount_point = Path::new(MOUNT_POINT);
    let device = OpenOptions::new().read(true).write(true).open(node)?;
    if !paths::is_mounted_at(&fs::read_to_string(MOUNTS)?, MOUNT_POINT) {
        if read_identity(&device)?.is_none() {
            shared.report(Progress::Formatting);
            log!("{}", to_foreign_line(&node.display().to_string()));
            format_device(&device, node)?;
        }
        allow_encryption(&device)?;
        fs::create_dir_all(mount_point)?;
        let source = node.to_str().ok_or_else(|| io::Error::other(UNREADABLE_NODE))?;
        sys::mount(source, mount_point, FILESYSTEM, "")?;
    }
    read_identity(&device)?.ok_or_else(|| io::Error::other(LOST_LABEL))
}

fn make_directories(mount_point: &Path, directories: &[&str]) -> io::Result<()> {
    directories.iter().try_for_each(|directory| fs::create_dir_all(mount_point.join(directory)))
}

// A restore's files take the partition's place between the safe's opening and the first part that reads the partition; then every public root is under the safe's key, and the directories are made, none of them before the safe is open; what failed is told with the error.
fn open_safe(shared: &Shared, uuid: &[u8; UUID_SIZE]) -> Result<(), (&'static str, io::Error)> {
    let mount_point = Path::new(MOUNT_POINT);
    restore::settle(mount_point).map_err(|e| (RESTORE_FILES, e))?;
    safe::open(shared, uuid).map_err(|e| (SAFE, e))?;
    restore::apply(mount_point).map_err(|e| (RESTORE_FILES, e))?;
    safe::enclose(shared, mount_point);
    make_directories(mount_point, &DIRECTORIES)
        .and_then(|()| make_directories(mount_point, &PRIVATE_DIRECTORIES))
        .map_err(|e| (SAFE, e))
}

pub(super) fn work(shared: &Shared) {
    let node = loop {
        match devnodes::partition_node(PARTITION) {
            Ok(node) => break node,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                shared.report(Progress::Waiting);
                shared.await_device();
                shared.enter(Progress::Looking);
            }
            Err(e) => {
                log!("data: {e}");
                return shared.report(Progress::Failed(e.to_string()));
            }
        }
    };
    let identity = match bring_up(&node, shared) {
        Ok(identity) => identity,
        Err(e) => {
            log!("data: {}: {e}", node.display());
            return shared.report(Progress::Failed(e.to_string()));
        }
    };
    shared.board.lock().set_id(identity.uuid);
    // Nothing is kept on the partition before the safe is open, and its directories are made only then.
    if let Err((what, e)) = open_safe(shared, &identity.uuid) {
        log!("data: {what}: {e}");
        return shared.report(Progress::Failed(e.to_string()));
    }
    let progress = Progress::from_capacity(sys::capacity(Path::new(MOUNT_POINT)).ok().map(|(bytes, _)| bytes));
    log!("{}", progress.to_mounted_line(&node.display().to_string()));
    shared.report(progress);
    safe::serve_changes(shared);
}

#[cfg(test)]
mod tests {
    use super::super::scratch;
    use super::*;

    #[test]
    fn blkdiscard_number() {
        assert_eq!(BLKDISCARD, 0x1277);
    }

    #[test]
    fn identity_by_label() {
        let root = scratch("ours");
        let path = root.join("disk");
        let request = Request {
            bytes: 1 << 30,
            start_sector: 0,
            label: LABEL,
            uuid: [7; 16],
            checkpoint: 1,
            kernel: "",
            time: 0,
            is_trimmed: true,
        };
        let image = f2fs::format(&request).unwrap();
        let foreign = f2fs::format(&Request { label: "someone", ..request }).unwrap();
        let write = |first: &[u8], second: &[u8]| {
            fs::write(&path, [first, second].concat()).unwrap();
            read_identity(&File::open(&path).unwrap()).unwrap().map(|identity| identity.uuid)
        };
        let blank = vec![0u8; BLOCK_SIZE];
        assert_eq!(write(&image.superblock, &image.superblock), Some([7; 16]));
        assert_eq!(write(&blank, &image.superblock), Some([7; 16]), "one intact copy is enough");
        assert_eq!(write(&foreign.superblock, &foreign.superblock), None);
        assert_eq!(write(&blank, &blank), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zero_runs() {
        let root = scratch("zero");
        let path = root.join("disk");
        let segment = u64::from(BLOCKS_PER_SEGMENT);
        let bytes = 3 * segment as usize * BLOCK_SIZE;
        fs::write(&path, vec![0xFF; bytes]).unwrap();
        let runs = [1..3, segment - 1..2 * segment + 5];
        zero(&OpenOptions::new().write(true).open(&path).unwrap(), &runs).unwrap();
        let content = fs::read(&path).unwrap();
        assert_eq!(content.len(), bytes, "nothing is written past the runs");
        for (number, block) in content.chunks(BLOCK_SIZE).enumerate() {
            let is_zero = runs.iter().any(|run| run.contains(&(number as u64)));
            assert!(block.iter().all(|&byte| byte == if is_zero { 0 } else { 0xFF }), "block {number}");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
