use crate::sys::devnodes;
use r1_image::ReadAt;
use r1_image::ext4::Ext4;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// The partition as the ext4 reader reads it.
pub struct Partition(File);

impl ReadAt for Partition {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(out, offset)
    }
}

/// Opens one of MediaTek's partitions of factory files (`nvdata`, `protect1`, `protect2`, `nvcfg`) as ext4.
pub fn files(partition: &str) -> io::Result<Ext4<Partition>> {
    Ext4::open(Partition(File::open(devnodes::partition_node(partition)?)?))
}
