use crate::sys::{self, Mapping, PAGE};
use r1_funcore::hw::{device_address, to_no_address_text};
use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

const DEVICE: &str = "/dev/ion";
const ION_IOC_ALLOC: u32 = sys::ioc(sys::IOC_BOTH, b'I', 0, size_of::<Allocation>());
const ION_IOC_FREE: u32 = sys::ioc(sys::IOC_BOTH, b'I', 1, size_of::<Handle>());
const ION_IOC_SHARE: u32 = sys::ioc(sys::IOC_BOTH, b'I', 4, size_of::<Share>());
const ION_IOC_CUSTOM: u32 = sys::ioc(sys::IOC_BOTH, b'I', 6, size_of::<Custom>());
const MULTIMEDIA_HEAP: u32 = 1 << 10;
const CACHED_NEEDS_SYNC: u32 = 1 | 2;
const ION_CMD_SYSTEM: u32 = 0;
const ION_CMD_MULTIMEDIA: u32 = 1;
const ION_MM_GET_IOVA: u32 = 8;
const ION_SYS_CACHE_SYNC: u32 = 0;
const ION_CACHE_INVALID_BY_RANGE: u32 = 1;
const ION_CACHE_FLUSH_BY_RANGE: u32 = 2;

#[repr(C)]
struct Allocation {
    len: u64,
    align: u64,
    heap_id_mask: u32,
    flags: u32,
    handle: i32,
}

#[repr(C)]
struct Handle {
    handle: i32,
}

#[repr(C)]
struct Share {
    handle: i32,
    fd: i32,
}

#[repr(C)]
struct Custom {
    cmd: u32,
    arg: u64,
}

#[repr(C)]
#[derive(Default)]
struct DeviceAddress {
    mm_cmd: u32,
    handle: u64,
    module_id: i32,
    security: u32,
    coherent: u32,
    reserve_iova_start: u32,
    reserve_iova_end: u32,
    phy_addr: u64,
    len: u64,
    rest_of_union: [u64; 3],
}

#[repr(C)]
struct CacheSync {
    sys_cmd: u32,
    handle: u64,
    va: u64,
    size: u32,
    sync_type: u32,
    iova: u64,
    rest_of_union: [u64; 10],
}

#[derive(Clone)]
pub struct Ion {
    file: Arc<File>,
    floor: RawFd,
}

impl Ion {
    pub(crate) fn open(floor: RawFd) -> io::Result<Ion> {
        Ok(Ion { file: Arc::new(sys::open_above(DEVICE, floor)?), floor })
    }

    fn custom<T>(&self, command: u32, data: &mut T) -> io::Result<()> {
        let mut custom = Custom { cmd: command, arg: std::ptr::from_mut(data) as u64 };
        unsafe { sys::ioctl(self.file.as_raw_fd(), ION_IOC_CUSTOM, &mut custom) }
    }

    /// A zeroed, mapped buffer of `bytes` rounded up to whole pages, with a device address for M4U port `port`.
    pub fn buffer(&self, bytes: usize, port: i32) -> io::Result<Buffer> {
        let (fd, bytes) = (self.file.as_raw_fd(), bytes.next_multiple_of(PAGE));
        let mut allocation = Allocation {
            len: bytes as u64,
            align: 0,
            heap_id_mask: MULTIMEDIA_HEAP,
            flags: CACHED_NEEDS_SYNC,
            handle: 0,
        };
        unsafe { sys::ioctl(fd, ION_IOC_ALLOC, &mut allocation)? };
        let memory = Memory { ion: self.clone(), handle: allocation.handle };
        let mut share = Share { handle: memory.handle, fd: -1 };
        unsafe { sys::ioctl(fd, ION_IOC_SHARE, &mut share)? };
        let share_fd = sys::raise_above(unsafe { OwnedFd::from_raw_fd(share.fd) }, self.floor)?;
        let map = sys::map(share_fd.as_raw_fd(), 0, bytes)?;
        let mut address = DeviceAddress {
            mm_cmd: ION_MM_GET_IOVA,
            handle: memory.handle as u32 as u64,
            module_id: port,
            ..DeviceAddress::default()
        };
        self.custom(ION_CMD_MULTIMEDIA, &mut address)?;
        let device_address = device_address(address.phy_addr, address.len, bytes as u64)
            .ok_or_else(|| io::Error::other(to_no_address_text(address.phy_addr, address.len)))?;
        let buffer = Buffer { map, share_fd, memory, bytes, device_address };
        // Pages that come out of the heap's pool were zeroed through the cache when they went in.
        buffer.flush(0..bytes)?;
        Ok(buffer)
    }
}

/// A mapped buffer; its fields drop in the order unmap, close, free.
pub struct Buffer {
    map: Mapping,
    share_fd: OwnedFd,
    memory: Memory,
    bytes: usize,
    device_address: u32,
}

struct Memory {
    ion: Ion,
    handle: i32,
}

impl Drop for Memory {
    fn drop(&mut self) {
        let mut handle = Handle { handle: self.handle };
        let _ = unsafe { sys::ioctl(self.ion.file.as_raw_fd(), ION_IOC_FREE, &mut handle) };
    }
}

impl Buffer {
    /// The address a hardware block reaches the first byte at.
    pub const fn device_address(&self) -> u32 {
        self.device_address
    }

    /// The size in bytes, whole pages.
    pub const fn len(&self) -> usize {
        self.bytes
    }

    pub(super) fn fd(&self) -> RawFd {
        self.share_fd.as_raw_fd()
    }

    #[track_caller]
    fn check_range(&self, range: &Range<usize>) {
        assert!(
            range.start <= range.end && range.end <= self.bytes,
            "range {range:?} of a buffer of {} bytes",
            self.bytes
        );
    }

    fn sync(&self, range: Range<usize>, sync_type: u32) -> io::Result<()> {
        self.check_range(&range);
        let mut sync = CacheSync {
            sys_cmd: ION_SYS_CACHE_SYNC,
            handle: self.memory.handle as u32 as u64,
            va: self.map.as_ptr() as u64 + range.start as u64,
            size: range.len() as u32,
            sync_type,
            iova: 0,
            rest_of_union: [0; 10],
        };
        self.memory.ion.custom(ION_CMD_SYSTEM, &mut sync)
    }

    pub(super) fn refresh(&self, range: Range<usize>) -> io::Result<()> {
        self.sync(range, ION_CACHE_INVALID_BY_RANGE)
    }

    /// Writes the CPU's cache of `range` to memory and drops it, so a DMA reads the CPU's writes and overwrites nothing.
    pub fn flush(&self, range: Range<usize>) -> io::Result<()> {
        self.sync(range, ION_CACHE_FLUSH_BY_RANGE)
    }

    /// The bytes of `range` as a DMA wrote them, the CPU's cache dropped first.
    pub fn dma_bytes(&self, range: Range<usize>) -> io::Result<&[u8]> {
        self.refresh(range.clone())?;
        Ok(self.bytes(range))
    }

    /// The bytes of `range`; a DMA's writes show only after `dma_bytes`.
    pub fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.check_range(&range);
        unsafe { std::slice::from_raw_parts(self.map.as_ptr().add(range.start), range.len()) }
    }

    /// The bytes of `range` to write; a DMA reads them only after `flush`.
    pub fn bytes_mut(&mut self, range: Range<usize>) -> &mut [u8] {
        self.check_range(&range);
        unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr().add(range.start), range.len()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(size_of::<Allocation>(), 32);
        assert_eq!(size_of::<Custom>(), 16);
        assert_eq!(size_of::<DeviceAddress>(), 80);
        assert_eq!(std::mem::offset_of!(DeviceAddress, module_id), 16);
        assert_eq!(std::mem::offset_of!(DeviceAddress, phy_addr), 40);
        assert_eq!(std::mem::offset_of!(DeviceAddress, len), 48);
        assert_eq!(size_of::<CacheSync>(), 120);
        assert_eq!(std::mem::offset_of!(CacheSync, va), 16);
        assert_eq!(std::mem::offset_of!(CacheSync, sync_type), 28);
        assert_eq!(ION_IOC_ALLOC, 0xc020_4900);
        assert_eq!(ION_IOC_FREE, 0xc004_4901);
        assert_eq!(ION_IOC_SHARE, 0xc008_4904);
        assert_eq!(ION_IOC_CUSTOM, 0xc010_4906);
    }
}
