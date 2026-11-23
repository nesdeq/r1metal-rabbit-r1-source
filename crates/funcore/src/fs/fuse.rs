//! The part of the FUSE protocol that serves the kernel one read-only file.

use crate::base::bytes::ne32;
use crate::base::bytes::ne64;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const MAJOR: u32 = 7;
const MINOR: u32 = 27;
/// The kernel hands no request to a reader with a smaller buffer.
pub const MIN_READ_BUFFER: usize = 8192;
const MAX_WRITE: u32 = 4096;
const IN_HEADER: usize = 40;
const OUT_HEADER: usize = 16;
const OPCODE_AT: usize = 4;
const UNIQUE_AT: usize = 8;
const NODE_AT: usize = 16;
const ROOT: u64 = 1;
const FILE: u64 = 2;
// linux/stat.h, linux/fcntl.h and dirent.h.
pub const S_IFDIR: u32 = 0o040_000;
pub const S_IFREG: u32 = 0o100_000;
pub const O_ACCMODE: u32 = 0o3;
pub const O_RDONLY: u32 = 0;
pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;
// asm-generic/errno.h.
pub const ENOENT: i32 = 2;
pub const EINVAL: i32 = 22;
pub const EROFS: i32 = 30;
pub const ENOSYS: i32 = 38;
pub const EPROTO: i32 = 71;
const DIRECTORY_MODE: u32 = S_IFDIR | 0o555;
const FILE_MODE: u32 = S_IFREG | 0o444;
const ATTRIBUTE_BLOCK: u64 = 512;
const MAX_NAME: u32 = 255;

const LOOKUP: u32 = 1;
const FORGET: u32 = 2;
const GETATTR: u32 = 3;
const OPEN: u32 = 14;
const READ: u32 = 15;
const STATFS: u32 = 17;
const RELEASE: u32 = 18;
const FLUSH: u32 = 25;
const INIT: u32 = 26;
const OPENDIR: u32 = 27;
const READDIR: u32 = 28;
const RELEASEDIR: u32 = 29;
const BATCH_FORGET: u32 = 42;

/// The mount's options over a connection descriptor.
pub fn to_mount_options(connection: i32) -> String {
    format!("fd={connection},rootmode={S_IFDIR:o},user_id=0,group_id=0")
}

/// The source of the bytes of the mount's file.
pub trait Source: Sync {
    fn size(&self) -> u64;
    fn read(&self, offset: u64, buffer: &mut [u8]);
}

impl<T: Source + ?Sized> Source for &T {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) {
        (**self).read(offset, buffer);
    }
}

impl<T: Source> Source for Option<T> {
    fn size(&self) -> u64 {
        self.as_ref().map_or(0, Source::size)
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) {
        match self {
            Some(content) => content.read(offset, buffer),
            None => buffer.fill(0),
        }
    }
}

/// The answer to a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    // The kernel expects no reply.
    Nothing,
    Data(Vec<u8>),
    // The file's bytes, in the connection's read buffer.
    Read,
    Error(i32),
}

#[derive(Default)]
struct Packet(Vec<u8>);

impl Packet {
    fn u16(mut self, value: u16) -> Self {
        self.0.extend(value.to_ne_bytes());
        self
    }

    fn u32(mut self, value: u32) -> Self {
        self.0.extend(value.to_ne_bytes());
        self
    }

    fn u64(mut self, value: u64) -> Self {
        self.0.extend(value.to_ne_bytes());
        self
    }

    fn zeros(mut self, count: usize) -> Self {
        self.0.resize(self.0.len() + count, 0);
        self
    }

    fn attributes(self, node: u64, size: u64) -> Self {
        let (mode, links) = if node == ROOT { (DIRECTORY_MODE, 2) } else { (FILE_MODE, 1) };
        let sizes = self.u64(node).u64(size).u64(size.div_ceil(ATTRIBUTE_BLOCK));
        sizes.zeros(3 * 8 + 3 * 4).u32(mode).u32(links).zeros(5 * 4)
    }
}

fn directory_entry(node: u64, next_offset: u64, name: &str) -> Vec<u8> {
    let kind = if node == ROOT { DT_DIR } else { DT_REG };
    let mut entry = Packet::default().u64(node).u64(next_offset).u32(name.len() as u32).u32(kind.into()).0;
    entry.extend(name.bytes());
    entry.resize(entry.len().next_multiple_of(8), 0);
    entry
}

/// A request's opcode, number, node and what follows its header; none for one cut short.
pub fn parse_request(bytes: &[u8]) -> Option<(u32, u64, u64, &[u8])> {
    let body = bytes.get(IN_HEADER..).unwrap_or_default();
    Some((ne32(bytes, OPCODE_AT)?, ne64(bytes, UNIQUE_AT)?, ne64(bytes, NODE_AT)?, body))
}

/// The answer to one request, `body` what follows its header; the bytes of a read go into `read`.
pub fn answer(opcode: u32, node: u64, body: &[u8], name: &str, content: &dyn Source, read: &mut Vec<u8>) -> Answer {
    let is_known = matches!(node, ROOT | FILE);
    match opcode {
        INIT => {
            let (Some(major), Some(minor), Some(readahead)) = (ne32(body, 0), ne32(body, 4), ne32(body, 8)) else {
                return Answer::Error(EINVAL);
            };
            if major < MAJOR {
                return Answer::Error(EPROTO);
            }
            let version = Packet::default().u32(MAJOR).u32(minor.min(MINOR));
            Answer::Data(version.u32(readahead).u32(0).u16(0).u16(0).u32(MAX_WRITE).u32(1).zeros(9 * 4).0)
        }
        LOOKUP if node == ROOT && body.strip_suffix(&[0]) == Some(name.as_bytes()) => {
            Answer::Data(Packet::default().u64(FILE).zeros(8 + 2 * 8 + 2 * 4).attributes(FILE, content.size()).0)
        }
        LOOKUP => Answer::Error(ENOENT),
        GETATTR if is_known => {
            // The file shows its size, the directory none.
            let size = if node == FILE { content.size() } else { 0 };
            Answer::Data(Packet::default().zeros(8 + 2 * 4).attributes(node, size).0)
        }
        OPEN if node == FILE => match ne32(body, 0) {
            Some(flags) if flags & O_ACCMODE == O_RDONLY => Answer::Data(Packet::default().zeros(8 + 2 * 4).0),
            _ => Answer::Error(EROFS),
        },
        OPENDIR if node == ROOT => Answer::Data(Packet::default().zeros(8 + 2 * 4).0),
        READ if node == FILE => {
            let (Some(offset), Some(size)) = (ne64(body, 8), ne32(body, 16)) else {
                return Answer::Error(EINVAL);
            };
            read.clear();
            read.resize(content.size().saturating_sub(offset).min(size.into()) as usize, 0);
            content.read(offset, read);
            Answer::Read
        }
        READDIR if node == ROOT => {
            let (Some(offset), Some(room)) = (ne64(body, 8), ne32(body, 16)) else {
                return Answer::Error(EINVAL);
            };
            let listing = [(ROOT, "."), (ROOT, ".."), (FILE, name)];
            let entries = listing
                .iter()
                .zip(1..)
                .skip(offset as usize)
                .map(|(&(node, name), next_offset)| directory_entry(node, next_offset, name));
            let mut data = Vec::new();
            for entry in entries {
                if data.len() + entry.len() > room as usize {
                    break;
                }
                data.extend(entry);
            }
            Answer::Data(data)
        }
        STATFS => Answer::Data(
            Packet::default()
                .zeros(5 * 8)
                .u32(ATTRIBUTE_BLOCK as u32)
                .u32(MAX_NAME)
                .u32(ATTRIBUTE_BLOCK as u32)
                .zeros(7 * 4)
                .0,
        ),
        RELEASE | RELEASEDIR | FLUSH if is_known => Answer::Data(Vec::new()),
        FORGET | BATCH_FORGET => Answer::Nothing,
        GETATTR | OPEN | OPENDIR | READ | READDIR | RELEASE | RELEASEDIR | FLUSH => Answer::Error(ENOENT),
        _ => Answer::Error(ENOSYS),
    }
}

/// The reply's header and data for request `unique`, a read's data from `read`; none if the kernel expects no reply.
pub fn reply<'a>(unique: u64, answer: &'a Answer, read: &'a [u8]) -> Option<([u8; OUT_HEADER], &'a [u8])> {
    let (error, data) = match answer {
        Answer::Nothing => return None,
        Answer::Data(data) => (0, data.as_slice()),
        Answer::Read => (0, read),
        Answer::Error(errno) => (-errno, &[][..]),
    };
    let mut header = [0; OUT_HEADER];
    header[..4].copy_from_slice(&((OUT_HEADER + data.len()) as u32).to_ne_bytes());
    header[4..8].copy_from_slice(&error.to_ne_bytes());
    header[8..].copy_from_slice(&unique.to_ne_bytes());
    Some((header, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const NAME: &str = "volume";
    const ATTR: usize = 88;
    const ENTRY_OUT: usize = 128;
    const ATTR_OUT: usize = 104;
    const INIT_OUT: usize = 64;
    const OPEN_OUT: usize = 16;
    const STATFS_OUT: usize = 80;
    const O_WRONLY: u32 = 1;
    const O_RDWR: u32 = 2;
    const O_LARGEFILE: u32 = 0o100_000;

    struct Counting(u64);

    impl Source for Counting {
        fn size(&self) -> u64 {
            self.0
        }

        fn read(&self, offset: u64, buffer: &mut [u8]) {
            buffer.iter_mut().zip(offset..).for_each(|(byte, at)| *byte = at as u8);
        }
    }

    fn ask(opcode: u32, node: u64, body: &[u8]) -> Answer {
        let mut read = Vec::new();
        match answer(opcode, node, body, NAME, &Counting(1000), &mut read) {
            Answer::Read => Answer::Data(read),
            other => other,
        }
    }

    fn data(answer: Answer) -> Vec<u8> {
        match answer {
            Answer::Data(data) => data,
            other => panic!("no data but {other:?}"),
        }
    }

    fn read_in(offset: u64, size: u32) -> Vec<u8> {
        Packet::default().u64(0).u64(offset).u32(size).zeros(4 + 8 + 2 * 4).0
    }

    #[test]
    fn mount_options() {
        assert_eq!(to_mount_options(7), "fd=7,rootmode=40000,user_id=0,group_id=0");
    }

    #[test]
    fn init_versions() {
        let init = |major: u32, minor: u32| {
            ask(INIT, 0, &Packet::default().u32(major).u32(minor).u32(131_072).u32(0xffff_ffff).0)
        };
        let reply = data(init(7, 31));
        assert_eq!(reply.len(), INIT_OUT);
        assert_eq!(
            (ne32(&reply, 0), ne32(&reply, 4), ne32(&reply, 8), ne32(&reply, 12)),
            (Some(7), Some(27), Some(131_072), Some(0))
        );
        assert_eq!(ne32(&reply, 20), Some(MAX_WRITE));
        assert_eq!(ne32(&data(init(7, 12)), 4), Some(12));
        assert_eq!(ne32(&data(init(8, 0)), 0), Some(7), "a newer kernel is told the version to ask with again");
        assert_eq!(init(6, 0), Answer::Error(EPROTO));
    }

    #[test]
    fn lookup_size() {
        let entry = data(ask(LOOKUP, ROOT, b"volume\0"));
        assert_eq!(entry.len(), ENTRY_OUT);
        let attributes = &entry[ENTRY_OUT - ATTR..];
        assert_eq!(
            (ne64(&entry, 0), ne64(attributes, 0), ne64(attributes, 8), ne64(attributes, 16)),
            (Some(FILE), Some(FILE), Some(1000), Some(2))
        );
        assert_eq!((ne32(attributes, 60), ne32(attributes, 64)), (Some(FILE_MODE), Some(1)));
        assert_eq!(entry[16..40], [0; 24], "nothing about it may be cached");
        assert_eq!(ask(LOOKUP, ROOT, b"other\0"), Answer::Error(ENOENT));
        assert_eq!(ask(LOOKUP, FILE, b"volume\0"), Answer::Error(ENOENT));
    }

    #[test]
    fn getattr_modes() {
        let (root, file) = (data(ask(GETATTR, ROOT, &[0; 16])), data(ask(GETATTR, FILE, &[0; 16])));
        assert_eq!((root.len(), file.len()), (ATTR_OUT, ATTR_OUT));
        let mode = |reply: &[u8]| ne32(&reply[ATTR_OUT - ATTR..], 60);
        assert_eq!((mode(&root), mode(&file)), (Some(DIRECTORY_MODE), Some(FILE_MODE)));
        assert_eq!(ne64(&root[ATTR_OUT - ATTR..], 8), Some(0));
        assert_eq!(ask(GETATTR, 3, &[0; 16]), Answer::Error(ENOENT));
    }

    #[test]
    fn open_read_only() {
        let open = |flags: u32| ask(OPEN, FILE, &Packet::default().u32(flags).u32(0).0);
        assert_eq!(open(O_RDONLY | O_LARGEFILE), Answer::Data(vec![0; OPEN_OUT]));
        assert_eq!(open(O_RDWR), Answer::Error(EROFS));
        assert_eq!(open(O_WRONLY), Answer::Error(EROFS));
        assert_eq!(ask(OPEN, ROOT, &[0; 8]), Answer::Error(ENOENT));
    }

    #[test]
    fn read_to_end() {
        assert_eq!(data(ask(READ, FILE, &read_in(254, 4))), [254, 255, 0, 1]);
        assert_eq!(data(ask(READ, FILE, &read_in(998, 4096))), [230, 231]);
        assert_eq!(data(ask(READ, FILE, &read_in(1000, 4096))), []);
        assert_eq!(data(ask(READ, FILE, &read_in(5000, 4096))), []);
        assert_eq!(ask(READ, FILE, &[0; 8]), Answer::Error(EINVAL));
        assert_eq!(ask(READ, ROOT, &read_in(0, 1)), Answer::Error(ENOENT));
        let mut read = Vec::new();
        answer(READ, FILE, &read_in(0, 4096), NAME, &Counting(1000), &mut read);
        let buffer = read.as_ptr();
        assert_eq!(answer(READ, FILE, &read_in(996, 4096), NAME, &Counting(1000), &mut read), Answer::Read);
        assert_eq!(
            (read.as_slice(), read.as_ptr()),
            (&[228, 229, 230, 231][..], buffer),
            "a later read goes into the buffer an earlier one made"
        );
    }

    #[test]
    fn readdir_entries() {
        let listing = data(ask(READDIR, ROOT, &read_in(0, 4096)));
        let names: Vec<&[u8]> = listing
            .as_chunks::<32>()
            .0
            .iter()
            .map(|entry| &entry[24..24 + ne32(entry, 16).unwrap() as usize])
            .collect();
        assert_eq!(names, [&b"."[..], b"..", b"volume"]);
        let (last, kind) = (&listing[64..], ne32(&listing[64..], 20));
        assert_eq!((ne64(last, 0), ne64(last, 8), kind), (Some(FILE), Some(3), Some(DT_REG.into())));
        assert_eq!(data(ask(READDIR, ROOT, &read_in(2, 4096))).len(), 32);
        assert_eq!(data(ask(READDIR, ROOT, &read_in(3, 4096))), []);
        assert_eq!(
            data(ask(READDIR, ROOT, &read_in(0, 40))).len(),
            32,
            "an entry that does not fit waits for the next request"
        );
    }

    #[test]
    fn release_and_forget() {
        assert_eq!(ask(RELEASE, FILE, &[0; 24]), Answer::Data(Vec::new()));
        assert_eq!(ask(FLUSH, FILE, &[0; 24]), Answer::Data(Vec::new()));
        assert_eq!(ask(RELEASEDIR, ROOT, &[0; 24]), Answer::Data(Vec::new()));
        assert_eq!(ask(FORGET, FILE, &[0; 8]), Answer::Nothing);
        assert_eq!(ask(BATCH_FORGET, 0, &[0; 8]), Answer::Nothing);
        assert_eq!(data(ask(STATFS, ROOT, &[])).len(), STATFS_OUT);
    }

    #[test]
    fn optional_content() {
        let mut buffer = [1u8; 4];
        let none: Option<Counting> = None;
        none.read(0, &mut buffer);
        assert_eq!((none.size(), buffer), (0, [0; 4]));
        let some = Some(Counting(1000));
        some.read(254, &mut buffer);
        assert_eq!((some.size(), Some(&Counting(5)).size(), buffer), (1000, 5, [254, 255, 0, 1]));
    }

    #[test]
    fn unsupported_opcodes() {
        const WRITE: u32 = 16;
        const SETATTR: u32 = 4;
        const INTERRUPT: u32 = 36;
        for opcode in [WRITE, SETATTR, INTERRUPT] {
            assert_eq!(ask(opcode, FILE, &[0; 64]), Answer::Error(ENOSYS));
        }
    }

    #[test]
    fn request_and_reply() {
        let mut bytes = Packet::default().u32(48).u32(READ).u64(77).u64(FILE).zeros(IN_HEADER - 24).0;
        bytes.extend_from_slice(&read_in(0, 4));
        let (opcode, unique, node, body) = parse_request(&bytes).unwrap();
        assert_eq!((opcode, unique, node, body.len()), (READ, 77, FILE, 40));
        assert_eq!(parse_request(&bytes[..20]), None, "cut short");
        let read = [1, 2, 3];
        let (header, data) = reply(77, &Answer::Read, &read).unwrap();
        assert_eq!(
            (ne32(&header, 0), ne32(&header, 4), ne64(&header, 8), data),
            (Some(19), Some(0), Some(77), &read[..])
        );
        let (header, data) = reply(78, &Answer::Error(ENOENT), &read).unwrap();
        assert_eq!((ne32(&header, 0), ne32(&header, 4), data.len()), (Some(16), Some(-ENOENT as u32), 0));
        assert_eq!(reply(79, &Answer::Nothing, &read), None, "a forgetting is answered with nothing");
    }
}
