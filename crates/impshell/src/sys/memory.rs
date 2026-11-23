use r1_funcore::hw::{Block, Hardware, locate};
use std::io;
use std::os::fd::RawFd;
use std::ptr::NonNull;
use std::time::Duration;

/// A shared read-write mapping; dropping it unmaps it.
pub struct Mapping {
    start: NonNull<u8>,
    bytes: usize,
}

// The mapping belongs to its one holder alone, so it may move to another thread.
unsafe impl Send for Mapping {}

impl Mapping {
    /// The first byte.
    pub const fn as_ptr(&self) -> *mut u8 {
        self.start.as_ptr()
    }

    #[track_caller]
    fn word(&self, at: usize) -> *mut u32 {
        assert!(
            at.is_multiple_of(size_of::<u32>()) && at + size_of::<u32>() <= self.bytes,
            "word {at:#x} of a mapping of {:#x} bytes",
            self.bytes
        );
        unsafe { self.start.as_ptr().add(at).cast() }
    }

    /// Reads a word as a register: volatile.
    #[track_caller]
    pub fn read(&self, at: usize) -> u32 {
        unsafe { self.word(at).read_volatile() }
    }

    /// Writes a word as a register: volatile.
    #[track_caller]
    pub fn write(&self, at: usize, value: u32) {
        unsafe { self.word(at).write_volatile(value) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.start.as_ptr().cast(), self.bytes) };
    }
}

/// Maps `bytes` of `fd` from `offset`, shared, for reading and writing.
pub fn map(fd: RawFd, offset: libc::off_t, bytes: usize) -> io::Result<Mapping> {
    let address = unsafe {
        libc::mmap(std::ptr::null_mut(), bytes, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, offset)
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let start = NonNull::new(address.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
    Ok(Mapping { start, bytes })
}

/// Register blocks in mappings of device memory.
#[derive(Default)]
pub struct Registers {
    mappings: Vec<Mapping>,
    blocks: Vec<(Block, usize, usize, usize)>,
}

impl Registers {
    /// Maps `fd` from `offset` as far as `blocks` reach, each block by its start and byte count.
    pub fn map(mut self, fd: RawFd, offset: libc::off_t, blocks: &[(Block, usize, usize)]) -> io::Result<Self> {
        let (index, length) = (self.mappings.len(), blocks.iter().map(|&(_, start, bytes)| start + bytes).max());
        self.mappings.push(map(fd, offset, length.unwrap_or(0))?);
        self.blocks.extend(blocks.iter().map(|&(block, start, bytes)| (block, index, start, bytes)));
        Ok(self)
    }
}

impl Hardware for Registers {
    fn read(&mut self, block: Block, offset: usize) -> u32 {
        let (index, at) = locate(&self.blocks, block, offset);
        self.mappings[index].read(at)
    }

    fn write(&mut self, block: Block, offset: usize, value: u32) {
        let (index, at) = locate(&self.blocks, block, offset);
        self.mappings[index].write(at, value);
    }

    fn pause(&mut self, micros: u32) {
        std::thread::sleep(Duration::from_micros(micros.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::temp_file;
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::AsRawFd;

    #[test]
    fn mapping_words_and_bounds() {
        let mut file = temp_file(&[0u8; 8]);
        let mapping = map(file.as_raw_fd(), 0, 8).unwrap();
        mapping.write(4, 0x1234_5678);
        assert_eq!((mapping.read(0), mapping.read(4)), (0, 0x1234_5678));
        drop(mapping);
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [0, 0, 0, 0, 0x78, 0x56, 0x34, 0x12], "what was written lies in what the mapping was of");
        let mapping = map(file.as_raw_fd(), 0, 8).unwrap();
        for outside in [8, 2] {
            assert!(std::panic::catch_unwind(|| mapping.read(outside)).is_err(), "a word at {outside}");
        }
    }
}
