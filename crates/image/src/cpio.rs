//! Writer for the newc cpio format the kernel unpacks as initramfs.

const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const ALIGN: usize = 4;
const BLOCK: usize = 512;
const TRAILER: &str = "TRAILER!!!";
const MAGIC: &[u8; 6] = b"070701";
// The magic and thirteen fields of eight hex digits.
const HEADER: usize = 110;
// Fields of a header by their place: the inode, the length of the data, the length of the name with its zero.
const INO: usize = 0;
const FILESIZE: usize = 6;
const NAMESIZE: usize = 11;

// A header's field at `index`; none for one that is no hex number.
fn field(header: &[u8], index: usize) -> Option<u32> {
    let digits = header.get(MAGIC.len() + 8 * index..MAGIC.len() + 8 * (index + 1))?;
    u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

/// An archive that goes on from the entries of `archive`, its trailer and what pads it dropped; none for bytes that are no archive with a trailer.
pub fn reopen(archive: &[u8]) -> Option<Archive> {
    let (mut at, mut last_ino) = (0, 0);
    loop {
        let header = archive.get(at..at + HEADER)?;
        if &header[..MAGIC.len()] != MAGIC {
            return None;
        }
        let (ino, size, name_size) = (field(header, INO)?, field(header, FILESIZE)?, field(header, NAMESIZE)?);
        let name = archive.get(at + HEADER..(at + HEADER + name_size as usize).checked_sub(1)?)?;
        if name == TRAILER.as_bytes() {
            return Some(Archive { out: archive[..at].to_vec(), last_ino });
        }
        last_ino = last_ino.max(ino);
        at = ((at + HEADER + name_size as usize).next_multiple_of(ALIGN) + size as usize).next_multiple_of(ALIGN);
    }
}

#[derive(Default)]
pub struct Archive {
    out: Vec<u8>,
    last_ino: u32,
}

impl Archive {
    fn pad(&mut self, to: usize) {
        self.out.resize(self.out.len().next_multiple_of(to), 0);
    }

    fn push(&mut self, ino: u32, path: &str, mode: u32, data: &[u8], (major, minor): (u32, u32)) {
        let nlink = if mode & S_IFDIR != 0 { 2 } else { 1 };
        let fields = [ino, mode, 0, 0, nlink, 0, data.len() as u32, 0, 0, major, minor, path.len() as u32 + 1, 0];
        self.out.extend_from_slice(MAGIC);
        for field in fields {
            self.out.extend_from_slice(format!("{field:08X}").as_bytes());
        }
        self.out.extend_from_slice(path.as_bytes());
        self.out.push(0);
        self.pad(ALIGN);
        self.out.extend_from_slice(data);
        self.pad(ALIGN);
    }

    fn add(&mut self, path: &str, mode: u32, data: &[u8], rdev: (u32, u32)) {
        self.last_ino += 1;
        self.push(self.last_ino, path, mode, data, rdev);
    }

    pub fn dir(&mut self, path: &str, perm: u32) {
        self.add(path, S_IFDIR | perm, &[], (0, 0));
    }

    pub fn file(&mut self, path: &str, perm: u32, data: &[u8]) {
        self.add(path, S_IFREG | perm, data, (0, 0));
    }

    pub fn char_device(&mut self, path: &str, perm: u32, major: u32, minor: u32) {
        self.add(path, S_IFCHR | perm, &[], (major, minor));
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.push(0, TRAILER, 0, &[], (0, 0));
        self.pad(BLOCK);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_layout() {
        let mut archive = Archive::default();
        archive.file("init", 0o755, b"abcde");
        archive.char_device("dev/console", 0o600, 5, 1);
        let out = archive.finish();
        // The inode, the mode, the links, the data's length, the device's numbers and the name's length.
        let header = |at: usize| [INO, 1, 4, FILESIZE, 9, 10, NAMESIZE].map(|index| field(&out[at..], index).unwrap());
        assert_eq!((&out[..6], &out[14..22]), (&b"070701"[..], &b"000081ED"[..]), "in capital hex digits");
        assert_eq!(header(0), [1, S_IFREG | 0o755, 1, 5, 0, 0, 5]);
        assert_eq!((&out[110..115], &out[116..121]), (&b"init\0"[..], &b"abcde"[..]));
        assert_eq!(header(124), [2, S_IFCHR | 0o600, 1, 0, 5, 1, 12], "device numbers in rdev");
        assert_eq!(out.len() % BLOCK, 0);
    }

    #[test]
    fn reopened_archive_appends() {
        let mut archive = Archive::default();
        archive.dir("dev", 0o755);
        archive.file("init", 0o755, b"abcde");
        archive.char_device("dev/console", 0o600, 5, 1);
        let first = archive.finish();
        let mut reopened = reopen(&first).unwrap();
        assert_eq!(reopened.last_ino, 3, "the next inode follows the last one used");
        reopened.file("flashed", 0o644, b"stamp 7\n");
        let both = reopened.finish();
        let mut expected = Archive::default();
        expected.dir("dev", 0o755);
        expected.file("init", 0o755, b"abcde");
        expected.char_device("dev/console", 0o600, 5, 1);
        expected.file("flashed", 0o644, b"stamp 7\n");
        assert!(both == expected.finish(), "as if it had been written in one go");
        assert_eq!(both.windows(TRAILER.len()).filter(|window| *window == TRAILER.as_bytes()).count(), 1);
        assert_eq!(reopen(&Archive::default().finish()).unwrap().finish(), Archive::default().finish());
    }

    #[test]
    fn foreign_bytes_not_reopened() {
        let whole = Archive::default().finish();
        assert!(reopen(b"").is_none());
        assert!(reopen(&[b'0'; 200]).is_none());
        assert!(reopen(&whole[..60]).is_none(), "a header cut short");
        let mut cut = Archive::default();
        cut.file("init", 0o755, b"abcde");
        let cut = cut.finish();
        assert!(reopen(&cut[..116]).is_none(), "an archive that ends before its trailer");
    }
}
