//! Files read out of an ext4 filesystem: enough of it to follow a path from the root and read what lies there.

use crate::{ReadAt, half, invalid, word};
use std::io;

const SUPERBLOCK_AT: u64 = 1024;
const SUPERBLOCK_BYTES: usize = 1024;
const MAGIC: u16 = 0xEF53;
// The smallest block, and the most the superblock may say it is doubled: blocks of 64 KiB.
const SMALLEST_BLOCK: u64 = 1024;
const MOST_DOUBLINGS: u32 = 6;
// The filesystem counts in 64 bits, so a group's descriptor may be longer than the 32 bytes it has otherwise.
const INCOMPAT_64BIT: u32 = 0x80;
const DESCRIPTOR_BYTES: u64 = 32;
// From where on a descriptor holds the upper halves of its block numbers.
const WIDE_DESCRIPTOR_BYTES: u64 = 64;
// As much of an inode as is read.
const INODE_BYTES: usize = 128;
const ROOT: u32 = 2;
// What kind of thing an inode is, in its mode.
const KIND: u16 = 0xF000;
const DIRECTORY: u16 = 0x4000;
const REGULAR: u16 = 0x8000;
// The inode says where its content lies by a tree of extents, whose nodes begin with a mark and have entries of twelve bytes behind a header as long.
const EXTENTS_FLAG: u32 = 0x8_0000;
const EXTENT_MAGIC: u16 = 0xF30A;
const EXTENT_BYTES: usize = 12;
// The deepest a tree of extents is, as the kernel's ext4 has it (`EXT4_MAX_EXTENT_DEPTH`).
const MOST_DEPTH: u16 = 5;
// Where in an inode the root of its tree lies, and the root's length.
const ROOT_AT: usize = 0x28;
const ROOT_BYTES: usize = 60;
// An extent said to be longer than this is as much shorter, and was set aside without ever being written: it reads as zeros.
const WRITTEN_MOST: u32 = 32768;
// A directory entry before its name.
const ENTRY_BYTES: usize = 8;

struct Inode {
    mode: u16,
    size: u64,
    flags: u32,
    // The root of the tree of extents.
    root: [u8; ROOT_BYTES],
}

// A run of a file: its first block in the file, how many blocks, its first block on the device, and whether it was ever written.
struct Extent {
    first: u64,
    blocks: u64,
    at: u64,
    written: bool,
}

pub struct Ext4<D> {
    device: D,
    block: u64,
    // The filesystem's size, which no file of it is larger than.
    bytes: u64,
    first_data_block: u64,
    inodes_per_group: u32,
    inode_bytes: u64,
    descriptor_bytes: u64,
}

fn beyond() -> io::Error {
    invalid("a filesystem that names a place past any device")
}

impl<D: ReadAt> Ext4<D> {
    /// Reads the superblock of a filesystem.
    pub fn open(mut device: D) -> io::Result<Ext4<D>> {
        let mut superblock = [0u8; SUPERBLOCK_BYTES];
        device.read_at(SUPERBLOCK_AT, &mut superblock)?;
        let (inodes_per_group, inode_bytes) = (word(&superblock, 0x28), u64::from(half(&superblock, 0x58)));
        let doublings = word(&superblock, 0x18);
        if half(&superblock, 0x38) != MAGIC
            || doublings > MOST_DOUBLINGS
            || inodes_per_group == 0
            || inode_bytes < INODE_BYTES as u64
        {
            return Err(invalid("no ext4 filesystem"));
        }
        let wide = word(&superblock, 0x60) & INCOMPAT_64BIT != 0;
        let descriptor_bytes =
            if wide { u64::from(half(&superblock, 0xFE)).max(DESCRIPTOR_BYTES) } else { DESCRIPTOR_BYTES };
        let blocks =
            u64::from(word(&superblock, 0x04)) | if wide { u64::from(word(&superblock, 0x150)) << 32 } else { 0 };
        let block = SMALLEST_BLOCK << doublings;
        Ok(Ext4 {
            device,
            block,
            bytes: blocks.checked_mul(block).ok_or_else(beyond)?,
            first_data_block: u64::from(word(&superblock, 0x14)),
            inodes_per_group,
            inode_bytes,
            descriptor_bytes,
        })
    }

    fn inode(&mut self, number: u32) -> io::Result<Inode> {
        let index = number.checked_sub(1).ok_or_else(|| invalid("a directory entry without an inode"))?;
        let (group, place) = (u64::from(index / self.inodes_per_group), u64::from(index % self.inodes_per_group));
        let mut descriptor = [0u8; WIDE_DESCRIPTOR_BYTES as usize];
        let read = self.descriptor_bytes.min(WIDE_DESCRIPTOR_BYTES) as usize;
        self.device.read_at(
            (self.first_data_block + 1) * self.block + group * self.descriptor_bytes,
            &mut descriptor[..read],
        )?;
        let table = u64::from(word(&descriptor, 0x08)) | u64::from(word(&descriptor, 0x28)) << 32;
        let at = table.checked_mul(self.block).and_then(|table| table.checked_add(place * self.inode_bytes));
        let mut inode = [0u8; INODE_BYTES];
        self.device.read_at(at.ok_or_else(beyond)?, &mut inode)?;
        let mut root = [0u8; ROOT_BYTES];
        root.copy_from_slice(&inode[ROOT_AT..ROOT_AT + ROOT_BYTES]);
        Ok(Inode {
            mode: half(&inode, 0),
            size: u64::from(word(&inode, 0x04)) | u64::from(word(&inode, 0x6C)) << 32,
            flags: word(&inode, 0x20),
            root,
        })
    }

    // Adds the extents under a node of an inode's tree, which stands `depth` levels over the leaves, up to the first that begins at `end` or past it, and says whether it met that one. They come in the order of the file's blocks from `next` on, each of a block at the least, and every node below the root holds one, as ext4 makes them, so that no tree is walked for longer than its file is.
    fn gather(
        &mut self,
        node: &[u8],
        depth: u16,
        end: u64,
        next: &mut u64,
        found: &mut Vec<Extent>,
    ) -> io::Result<bool> {
        let entries = usize::from(half(node, 2));
        if half(node, 0) != EXTENT_MAGIC || half(node, 6) != depth || (entries + 1) * EXTENT_BYTES > node.len() {
            return Err(invalid("a damaged tree of extents"));
        }
        for entry in node[EXTENT_BYTES..].as_chunks::<EXTENT_BYTES>().0.iter().take(entries) {
            if depth == 0 {
                let first = u64::from(word(entry, 0));
                let said = u32::from(half(entry, 4));
                let written = said <= WRITTEN_MOST;
                let blocks = u64::from(if written { said } else { said - WRITTEN_MOST });
                if blocks == 0 || first < *next {
                    return Err(invalid("a tree of extents out of the order of its file's blocks"));
                }
                if first >= end {
                    return Ok(true);
                }
                *next = first + blocks;
                found.push(Extent {
                    first,
                    blocks,
                    at: u64::from(word(entry, 8)) | u64::from(half(entry, 6)) << 32,
                    written,
                });
            } else {
                let mut below = vec![0u8; self.block as usize];
                self.device
                    .read_at((u64::from(word(entry, 4)) | u64::from(half(entry, 8)) << 32) * self.block, &mut below)?;
                if half(&below, 2) == 0 {
                    return Err(invalid("a node of a tree of extents that holds nothing"));
                }
                if self.gather(&below, depth - 1, end, next, found)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn content(&mut self, inode: &Inode) -> io::Result<Vec<u8>> {
        if inode.flags & EXTENTS_FLAG == 0 {
            return Err(invalid("a file that is not kept in extents"));
        }
        let depth = half(&inode.root, 6);
        if depth > MOST_DEPTH {
            return Err(invalid("a tree of extents deeper than ext4 makes one"));
        }
        if inode.size > self.bytes {
            return Err(invalid("a file larger than its filesystem"));
        }
        let mut extents = Vec::new();
        self.gather(&inode.root, depth, inode.size.div_ceil(self.block), &mut 0, &mut extents)?;
        let mut content = vec![0u8; usize::try_from(inode.size).map_err(|_| invalid("a file too large to read"))?];
        for extent in extents.iter().filter(|extent| extent.written) {
            let begin = (extent.first * self.block).min(inode.size) as usize;
            let end = ((extent.first + extent.blocks) * self.block).min(inode.size) as usize;
            self.device.read_at(extent.at * self.block, &mut content[begin..end])?;
        }
        Ok(content)
    }

    fn entries(content: &[u8]) -> Vec<(u32, &[u8])> {
        let (mut at, mut found) = (0, Vec::new());
        while at + ENTRY_BYTES <= content.len() {
            let (inode, length, name) =
                (word(content, at), usize::from(half(content, at + 4)), usize::from(content[at + 6]));
            if length < ENTRY_BYTES || at + ENTRY_BYTES + name > content.len() {
                break;
            }
            if inode != 0 {
                found.push((inode, &content[at + ENTRY_BYTES..at + ENTRY_BYTES + name]));
            }
            at += length;
        }
        found
    }

    fn find(&mut self, path: &str) -> io::Result<Inode> {
        let mut inode = self.inode(ROOT)?;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if inode.mode & KIND != DIRECTORY {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{path}: {part} lies in something that is no directory"),
                ));
            }
            let content = self.content(&inode)?;
            let found = Self::entries(&content)
                .into_iter()
                .find(|(_, name)| *name == part.as_bytes())
                .map(|(number, _)| number);
            inode = self
                .inode(found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{path}: no {part}")))?)?;
        }
        Ok(inode)
    }

    /// The content of the file at a path.
    pub fn read(&mut self, path: &str) -> io::Result<Vec<u8>> {
        let inode = self.find(path)?;
        if inode.mode & KIND != REGULAR {
            return Err(invalid(&format!("{path} is no file")));
        }
        self.content(&inode)
    }

    /// The entries of the directory at a path, each with its kind, but for the two that every directory has.
    pub fn list(&mut self, path: &str) -> io::Result<Vec<(String, Kind)>> {
        let inode = self.find(path)?;
        if inode.mode & KIND != DIRECTORY {
            return Err(invalid(&format!("{path} is no directory")));
        }
        let content = self.content(&inode)?;
        let mut entries = Vec::new();
        for (number, name) in Self::entries(&content) {
            let name = String::from_utf8_lossy(name).into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let kind = match self.inode(number)?.mode & KIND {
                DIRECTORY => Kind::Directory,
                REGULAR => Kind::Regular,
                _ => Kind::Other,
            };
            entries.push((name, kind));
        }
        Ok(entries)
    }
}

/// What an entry of a directory is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Regular,
    Directory,
    Other,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const BLOCK: usize = 1024;
    const INODE_TABLE: usize = 5;

    fn put(image: &mut [u8], at: usize, bytes: &[u8]) {
        image[at..at + bytes.len()].copy_from_slice(bytes);
    }

    // Writes an inode whose content lies in those extents, each (first block in the file, blocks, first block on the device).
    fn inode(image: &mut [u8], number: usize, mode: u16, size: u32, extents: &[(u32, u16, u32)]) {
        let at = INODE_TABLE * BLOCK + (number - 1) * INODE_BYTES;
        put(image, at, &mode.to_le_bytes());
        put(image, at + 0x04, &size.to_le_bytes());
        put(image, at + 0x20, &EXTENTS_FLAG.to_le_bytes());
        node(image, at + ROOT_AT, 0, extents);
    }

    // Writes a node of a tree of extents: leaves for depth 0, else pointers to the blocks of the nodes below.
    fn node(image: &mut [u8], at: usize, depth: u16, entries: &[(u32, u16, u32)]) {
        put(image, at, &EXTENT_MAGIC.to_le_bytes());
        put(image, at + 2, &(entries.len() as u16).to_le_bytes());
        put(image, at + 6, &depth.to_le_bytes());
        for (index, (first, blocks, block)) in entries.iter().enumerate() {
            let entry = at + (index + 1) * EXTENT_BYTES;
            put(image, entry, &first.to_le_bytes());
            if depth == 0 {
                put(image, entry + 4, &blocks.to_le_bytes());
                put(image, entry + 8, &block.to_le_bytes());
            } else {
                put(image, entry + 4, &block.to_le_bytes());
            }
        }
    }

    // Writes a directory's block: each name with its inode, the last entry reaching to the block's end.
    fn directory(image: &mut [u8], block: usize, names: &[(&str, u32)]) {
        let mut at = block * BLOCK;
        for (index, (name, number)) in names.iter().enumerate() {
            let length = if index + 1 == names.len() {
                (block + 1) * BLOCK - at
            } else {
                (ENTRY_BYTES + name.len()).next_multiple_of(4)
            };
            put(image, at, &number.to_le_bytes());
            put(image, at + 4, &(length as u16).to_le_bytes());
            image[at + 6] = name.len() as u8;
            put(image, at + ENTRY_BYTES, name.as_bytes());
            at += length;
        }
    }

    // A filesystem of 64 blocks: `firmware/a.bin` in one extent, `firmware/tree.bin` under a tree of two levels with a block never written, and the file `notes`.
    pub(crate) fn image() -> Vec<u8> {
        let mut image = vec![0u8; 64 * BLOCK];
        let superblock = SUPERBLOCK_AT as usize;
        put(&mut image, superblock + 0x04, &64u32.to_le_bytes());
        put(&mut image, superblock + 0x14, &1u32.to_le_bytes());
        put(&mut image, superblock + 0x28, &32u32.to_le_bytes());
        put(&mut image, superblock + 0x38, &MAGIC.to_le_bytes());
        put(&mut image, superblock + 0x58, &(INODE_BYTES as u16).to_le_bytes());
        put(&mut image, 2 * BLOCK + 0x08, &(INODE_TABLE as u32).to_le_bytes());
        inode(&mut image, ROOT as usize, DIRECTORY, BLOCK as u32, &[(0, 1, 20)]);
        directory(&mut image, 20, &[(".", 2), ("..", 2), ("firmware", 12), ("gone", 0), ("notes", 15)]);
        inode(&mut image, 12, DIRECTORY, BLOCK as u32, &[(0, 1, 21)]);
        directory(&mut image, 21, &[(".", 12), ("..", 2), ("a.bin", 13), ("tree.bin", 14)]);
        inode(&mut image, 13, REGULAR, 1500, &[(0, 2, 30)]);
        image[30 * BLOCK..32 * BLOCK].iter_mut().enumerate().for_each(|(index, byte)| *byte = index as u8);
        inode(&mut image, 14, REGULAR, 3000, &[]);
        node(&mut image, INODE_TABLE * BLOCK + 13 * INODE_BYTES + ROOT_AT, 1, &[(0, 0, 40)]);
        node(&mut image, 40 * BLOCK, 0, &[(0, 1, 41), (1, WRITTEN_MOST as u16 + 1, 42), (2, 1, 43)]);
        image[41 * BLOCK..44 * BLOCK].fill(0x5A);
        inode(&mut image, 15, REGULAR, 3, &[(0, 1, 44)]);
        put(&mut image, 44 * BLOCK, b"hey");
        image
    }

    #[test]
    fn file_read_by_path() {
        let image = image();
        let mut files = Ext4::open(&image[..]).unwrap();
        assert_eq!(
            files.list("").unwrap(),
            [("firmware".into(), Kind::Directory), ("notes".into(), Kind::Regular)],
            "an entry without an inode is none"
        );
        assert_eq!(
            files.list("/firmware/").unwrap(),
            [("a.bin".into(), Kind::Regular), ("tree.bin".into(), Kind::Regular)]
        );
        let plain = files.read("firmware/a.bin").unwrap();
        assert!(
            plain.len() == 1500 && plain.iter().enumerate().all(|(index, &byte)| byte == index as u8),
            "as long as the inode says, not as its blocks are"
        );
        let tree = files.read("/firmware/tree.bin").unwrap();
        assert_eq!(tree.len(), 3000);
        assert!(
            tree[..1024].iter().all(|&byte| byte == 0x5A)
                && tree[1024..2048].iter().all(|&byte| byte == 0)
                && tree[2048..].iter().all(|&byte| byte == 0x5A),
            "a block that was set aside and never written reads as zeros"
        );
        assert_eq!(files.read("notes").unwrap(), b"hey");
        let mut kept = image.clone();
        inode(&mut kept, 15, REGULAR, 3, &[(0, 1, 44), (1, WRITTEN_MOST as u16 + 4, 45)]);
        assert_eq!(
            Ext4::open(&kept[..]).unwrap().read("notes").unwrap(),
            b"hey",
            "blocks set aside past the file's end are no part of it"
        );
    }

    #[test]
    fn missing_and_damaged_refused() {
        let image = image();
        let mut files = Ext4::open(&image[..]).unwrap();
        assert_eq!(files.read("firmware/b.bin").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(files.read("notes/a.bin").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(files.read("firmware").is_err() && files.list("notes").is_err() && files.read("gone").is_err());
        assert!(Ext4::open(&[0u8; 4096][..]).is_err() && Ext4::open(&[0u8; 100][..]).is_err());
        let refused = |change: &dyn Fn(&mut Vec<u8>), path: &str| {
            let mut changed = image.clone();
            change(&mut changed);
            Ext4::open(&changed[..]).unwrap().read(path).is_err()
        };
        let [a, tree, notes] = [12, 13, 14].map(|index| INODE_TABLE * BLOCK + index * INODE_BYTES);
        assert!(refused(&|image| image[a + ROOT_AT] = 0, "firmware/a.bin"), "a tree of extents without its mark");
        let deep = (MOST_DEPTH + 1).to_le_bytes();
        assert!(refused(&|image| put(image, notes + ROOT_AT + 6, &deep), "notes"), "a tree deeper than ext4 makes one");
        let large = |image: &mut Vec<u8>| put(image, notes + 0x6C, &u32::MAX.to_le_bytes());
        assert!(refused(&large, "notes"), "a file larger than its filesystem");
        let again = |image: &mut Vec<u8>| node(image, tree + ROOT_AT, 1, &[(0, 0, 40), (3, 0, 40)]);
        assert!(refused(&again, "firmware/tree.bin"), "a tree that names a node twice names its runs out of order");
        let empty = |image: &mut Vec<u8>| node(image, 40 * BLOCK, 0, &[]);
        assert!(refused(&empty, "firmware/tree.bin"), "a node below the root of nothing");
        let naught = |image: &mut Vec<u8>| node(image, notes + ROOT_AT, 0, &[(0, 0, 44)]);
        assert!(refused(&naught, "notes"), "an extent of no blocks");
    }
}
